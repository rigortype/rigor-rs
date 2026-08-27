#!/usr/bin/env python3
"""Project-level EFFECT-SUMMARY differential: rigor-rs vs the pinned reference.

The instrument [ADR-0043](../docs/adr/0043-effect-system-port-parity-model.md)
specifies. `fp_audit.py` grades `rigor check` as a SET of `(path, line, column,
rule)` keys; an effect summary is not in that shape — it is keyed by a method,
carries a proven lane, a declared lane and an exhaustiveness bit, and only exists
for a PROJECT (`rigor effects` reads `.rigor.yml` and analyses the project's own
call graph as a closed world).

So this tool differs from every other corpus tool here in one structural way:
**the unit is a project directory and each arm runs IN it**, rather than in a
fresh temp cwd with no config. That is the point — the config is what turns
collection on and what scopes the closed world. It also makes this the first
instrument in the repo that sees project `sig/`, `.rigor.yml` and plugins, which
the standing sweep structurally cannot.

Verdicts (ADR-0043 § 4), per method:

  MATCH              the lanes agree
  UNDER              rigor-rs claims LESS: method absent, proven label missing,
                     or more taint. Expected; the arc's odometer; never fatal.
  OVER               rigor-rs claims MORE: method the oracle does not report, a
                     proven label it does not prove, or exhaustiveness it does
                     not claim. THE GATE — an extra proven label is a false
                     positive in waiting, because the proven lane is the only
                     lane a verdict may read.
  DECLARED-MISMATCH  the declared (`≤`) lane differs. It is copied from the
                     author's annotation, not inferred, so a difference is a
                     parse bug, not a coverage gap. Also fatal.

Both arms run with `--full`. The default report OMITS methods it considers pure,
so grading the default would let the port hide a whole class of disagreement by
simply not reporting a method — the omission and a genuine ∅ summary are
indistinguishable there.

Exit 0 iff every project compared cleanly (0 OVER, 0 DECLARED-MISMATCH) and no
comparison was INVALID.

**A SECOND comparison runs beside it: the SNAPSHOT** (`.rigor-effects.yml`,
ADR-0043 § 3). It is reported and totalled separately, because it grades a
different artifact under an inverted soundness direction — see "THE SNAPSHOT
SURFACE" below and issue #116. Today it does not contribute to the exit code;
`--snapshot-gate` promotes it, `--no-snapshot` skips it.

The standing set is the hand-written fixture projects under
`harness/effects-corpus/` **plus a REAL project** (`REAL_PROJECTS` below). The
fixtures alone cannot fail a slice: an arm emitting no `unresolved-self-call`
taint whatsoever scored byte-identically to a tuned one on all seven of them
(`docs/notes/20260826-effects-s4-probe.md` § 6), which is the fixture-corpus
blind spot in its purest form. A corpus the port's own authors wrote can only
contain shapes they thought of.

Usage:
  effects_diff.py [PROJECT_DIR ...]     # default: fixtures + default real set
  effects_diff.py --scale               # …and the opt-in large corpora too
  effects_diff.py --list                # what would be measured
  effects_diff.py --self-test           # grade (and re-derive) the oracle against itself
  effects_diff.py --snapshot-gate       # let the snapshot comparison set the exit code
  effects_diff.py --no-snapshot         # report comparison only
"""
import argparse
import contextlib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REF_DIR = os.environ.get("REFERENCE_RIGOR_DIR", os.path.join(REPO, "reference/rigor"))
REF_LIB = os.path.join(REF_DIR, "lib")
REF_EXE = os.path.join(REF_DIR, "exe", "rigor")
DEFAULT_CORPUS = os.path.join(REPO, "harness", "effects-corpus")
SWEEP_MANIFEST = os.environ.get(
    "SWEEP_CORPORA", os.path.join(REPO, "harness", "sweep-corpora.yml")
)

# --------------------------------------------------------------------------
# The REAL projects in the standing set.
#
# Selection and rationale live here; the PATHS do not. Each entry names a label
# from `harness/sweep-corpora.yml`, which stays the repo's single membership
# list for "which external checkouts do we measure" — the drift `run_corpus.rb`
# and `fp_audit.py` already share that file to avoid. A label this table names
# that the manifest does not carry is a REPO bug and hard-fails; a corpus the
# manifest carries that is not on this MACHINE is SKIPPED, loudly.
#
# `opt_in` keeps the default run at fixture speed plus roughly ten seconds.
# --------------------------------------------------------------------------
REAL_PROJECTS = [
    {
        "label": "mastodon/app",
        "opt_in": False,
        "why": "1,236 files / ~6,948 methods of Rails application code, and the "
               "cheapest real project in the set (~10 s). Contains the shapes no "
               "fixture author writes on purpose: `elsif` arms, `return` values, "
               "kwarg callees, blocks nested in blocks.",
    },
    {
        "label": "gitlab-foss/lib",
        "opt_in": True,
        "why": "28,607 methods and ~40 s. Every slice-4 arm that was 0 OVER on "
               "the fixtures AND on mastodon still leaked here — 111, 29 and 5 "
               "OVER for three successively stricter rules "
               "(20260826-effects-s4-probe.md § 5c). Opt-in on run time alone; "
               "it is the strongest member of the set.",
    },
]

# Header keys excluded from the snapshot comparison. NARROWED from four keys to
# one on 2026-08-28 (issue #116, the slice-5 probe § 1a / § 7): of the four
# fields `Snapshot.build` writes, only `rigor:` NECESSARILY differs between the
# two implementations. Excluding the other three discarded three real
# pin-tracking facts, and excluding `schema:` in particular hid the one field
# whose entire job is to say "an older reader would misread this file".
#
#   rigor:          EXCLUDED — `Rigor::VERSION` is `0.3.4` and the port's is
#                   `0.0.1`. Nothing can make these agree and nothing should.
#   schema:         COMPARED — `Snapshot::SCHEMA`, a constant. The port ships no
#                   writer, so `PORT_SNAPSHOT_SCHEMA` below is the grader
#                   asserting the value on its behalf; a pin that bumps SCHEMA
#                   fails here, which is the notification we want.
#   vocabulary:     COMPARED — read from the port's OWN vendored
#                   `crates/rigor-effects/vendor/effects/registry.yml`, so this
#                   is a genuine port-side fact and the vendored catalogue's
#                   third pin-tracking gate.
#   config_digest:  COMPARED — recomputed grader-side from the project's parsed
#                   `effects:` block with upstream's recipe (`identity.rb:90`).
#                   See `config_digest()` for what that does and does not catch.
SNAPSHOT_HEADER_KEYS = {"rigor"}

LAST_RS_FAILURE = None


# --------------------------------------------------------------------------
# The measured rigor-rs binary. Same contract as fp_audit.py / gap_census.py:
# auto-built when absent, REFUSED when older than the newest crates/ source, and
# its path + build time printed in the header. A six-day-old release binary once
# made two merged slices read as "closed nothing"
# (docs/notes/20260807-fp-audit-port-side-blind-spots.md).
# --------------------------------------------------------------------------
def crate_source_dirs():
    """The crate directories the measured binary is actually BUILT FROM.

    The `rigor-cli` path-dependency closure, read out of the manifests — NOT
    every `crates/*` entry. `crates/*` is a workspace GLOB, and a member nothing
    links is not an input to the binary, so scanning one would make the binary
    read as PERMANENTLY stale: cargo never rebuilds `rigor` for it.

    `rigor-effects` was exactly that case through ADR-0043 slice 1, when nothing
    linked it. Slice 2 gave it a consumer — `rigor-cli`'s `effects` collector —
    and the crate therefore RE-ENTERED this scan on its own, with no edit here,
    which is the whole point of deriving the set instead of listing it. Its
    `vendor/effects/*.yml` are `include_str!`d, so a re-vendored catalogue now
    correctly reads as a stale binary. Derived, never an exclusion list; twin of
    `harness/fp_audit.py`'s.
    """
    root = os.path.join(REPO, "crates")
    seen, queue = {}, ["rigor-cli"]
    while queue:
        name = queue.pop(0)
        if name in seen:
            continue
        path = os.path.join(root, name)
        if not os.path.isdir(path):
            continue
        seen[name] = path
        manifest = os.path.join(path, "Cargo.toml")
        if not os.path.isfile(manifest):
            continue
        with open(manifest, encoding="utf-8") as handle:
            queue.extend(re.findall(r'path\s*=\s*"\.\./([\w.-]+)"', handle.read()))
    return sorted(seen.values())


def newest_crates_mtime():
    newest, newest_path = 0.0, None
    for crate in crate_source_dirs():
        for root, dirs, names in os.walk(crate):
            dirs[:] = [d for d in dirs if d != "target"]
            for n in names:
                p = os.path.join(root, n)
                try:
                    m = os.path.getmtime(p)
                except OSError:
                    continue
                if m > newest:
                    newest, newest_path = m, p
    return newest, newest_path


def resolve_rs_binary():
    explicit = os.environ.get("RIGOR_RS_BIN")
    path = explicit or os.path.join(REPO, "target", "release", "rigor")
    if not os.path.exists(path):
        if explicit:
            sys.exit(f"ERROR: RIGOR_RS_BIN={path} does not exist.")
        print("rigor-rs binary absent — building (cargo build --offline --release -p rigor-cli)")
        subprocess.run(["cargo", "build", "--offline", "--release", "-p", "rigor-cli"],
                       cwd=REPO, check=True)
    built = os.path.getmtime(path)
    print(f"rigor-rs binary: {path}")
    print(f"  built:                 {time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(built))}")
    if explicit and not path.startswith(os.path.join(REPO, "target")):
        print("  (explicit RIGOR_RS_BIN outside target/ — staleness NOT checked)")
        return path
    newest, newest_path = newest_crates_mtime()
    rel = os.path.relpath(newest_path, REPO) if newest_path else "?"
    print(f"  newest crates/ source: "
          f"{time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(newest))}  ({rel})")
    if built < newest:
        sys.exit("\nERROR: STALE BINARY — the measurement would describe a binary that is not\n"
                 "       this working tree. Rebuild first:\n"
                 "         cargo build --offline --release -p rigor-cli")
    return path


# --------------------------------------------------------------------------
# The two arms. Both run IN the project directory (ADR-0043 § 4). `rigor
# effects` accepts NO --no-cache flag (probed 2026-08-26: `invalid option`,
# exit 64), so the reference arm CLEARS the project's `.rigor/cache` before
# and after each run instead: the persistent cache is keyed by cwd and here
# the cwd is the project rather than a fresh temp dir (UPSTREAM.md hazard 2),
# and a committed or cross-host cache entry would otherwise be read silently.
# The after-clear also keeps corpus checkouts residue-free (a self-test run
# used to leave 28 untracked cache directories behind).
#
# Both return None (NOT {}) when the arm produced nothing parseable. An empty
# PORT result makes the OVER count 0 by construction — the exact shape that let
# a crashing binary pass this project's central gate once already — and an empty
# REFERENCE result turns every port method into a phantom OVER.
# --------------------------------------------------------------------------
def _parse_methods(stdout):
    i = stdout.find("{")
    if i < 0:
        return None
    try:
        obj = json.loads(stdout[i:])
    except Exception:
        return None
    methods = obj.get("methods")
    return methods if isinstance(methods, dict) else None


def _clear_ref_cache(project):
    # `.rigor/cache` only — `.rigor/` itself is left alone in case a corpus
    # project ever carries non-cache state there.
    shutil.rmtree(os.path.join(project, ".rigor", "cache"), ignore_errors=True)


def run_ref(project):
    # The bundled rigor-rbs-inline lib is pinned onto the load path (UPSTREAM.md
    # hazard 1 / upstream #194): the ADR-93 auto-wire otherwise resolves a stale
    # INSTALLED rigortype gem's pre-gate plugin copy and poisons the comparison.
    ref_plugin = os.path.join(REF_DIR, "plugins", "rigor-rbs-inline", "lib")
    _clear_ref_cache(project)
    try:
        r = subprocess.run(["ruby", "-I", REF_LIB, "-I", ref_plugin,
                            REF_EXE, "effects", "--full", "--format=json"],
                           capture_output=True, text=True, cwd=project,
                           stdin=subprocess.DEVNULL)
    finally:
        _clear_ref_cache(project)
    return _parse_methods(r.stdout)


def run_rs(rs_bin, project):
    global LAST_RS_FAILURE
    LAST_RS_FAILURE = None
    try:
        r = subprocess.run([rs_bin, "effects", "--full", "--format=json"],
                           capture_output=True, text=True, cwd=project,
                           stdin=subprocess.DEVNULL)
    except OSError as e:
        LAST_RS_FAILURE = str(e)
        return None
    # `rigor effects` is a report, not a check: it has no findings exit code, so
    # anything but 0 is a failure — EXCEPT the one case this tool has to
    # distinguish, "the port does not have the subcommand yet". That is reported
    # as NOT-IMPLEMENTED with an empty summary map rather than as an INVALID
    # comparison, so the debt baseline is measurable before slice 1 exists.
    #
    # Matched on the MESSAGE, not the exit code: rigor-rs answers an unknown
    # subcommand with `rigor-rs: unknown command \`x\`` and exit 2, and 2 is not
    # a documented usage code (64 is), so keying on it would silently swallow
    # some future real failure that happens to exit 2.
    combined = (r.stdout or "") + (r.stderr or "")
    if "unknown command" in combined:
        LAST_RS_FAILURE = "NOT-IMPLEMENTED"
        return {}
    if r.returncode != 0:
        LAST_RS_FAILURE = f"exit {r.returncode}"
        return None
    return _parse_methods(r.stdout)


# --------------------------------------------------------------------------
# Comparison
# --------------------------------------------------------------------------
def lanes(entry):
    """The three graded lanes of one method's summary, normalised.

    `effects` is the proven lane (the transitive one the report prints);
    `declared` is copied from the author's annotation; `exhaustive` is the taint
    bit. A missing key reads as the WEAKEST value, so a shape this tool does not
    understand can only ever produce UNDER, never a phantom OVER.
    """
    if not isinstance(entry, dict):
        return set(), set(), False
    proven = set(entry.get("effects") or [])
    declared = set(entry.get("declared") or [])
    exhaustive = bool(entry.get("exhaustive", False))
    return proven, declared, exhaustive


def compare(ref, rs):
    """Per-method verdicts. Returns (counter, findings) where findings are the
    OVER / DECLARED-MISMATCH rows — the fatal ones, listed in full."""
    verdicts = Counter()
    findings = []
    for name in sorted(set(ref) | set(rs)):
        if name not in rs:
            verdicts["UNDER"] += 1
            verdicts["under:absent-method"] += 1
            continue
        if name not in ref:
            verdicts["OVER"] += 1
            findings.append(("OVER", name, "method the oracle does not report"))
            continue
        rp, rd, rex = lanes(ref[name])
        sp, sd, sex = lanes(rs[name])
        over_labels = sp - rp
        under_labels = rp - sp
        if over_labels:
            verdicts["OVER"] += 1
            findings.append(("OVER", name, f"proven labels not proven by the oracle: "
                                           f"{sorted(over_labels)}"))
        if sex and not rex:
            verdicts["OVER"] += 1
            findings.append(("OVER", name, "claims exhaustiveness the oracle does not"))
        if sd != rd:
            verdicts["DECLARED-MISMATCH"] += 1
            findings.append(("DECLARED-MISMATCH", name,
                             f"declared lane {sorted(sd)} != oracle {sorted(rd)}"))
        if under_labels:
            verdicts["UNDER"] += 1
            verdicts["under:missing-label"] += 1
        elif rex and not sex:
            verdicts["UNDER"] += 1
            verdicts["under:extra-taint"] += 1
        elif not over_labels and sd == rd and sex == rex:
            verdicts["MATCH"] += 1
    return verdicts, findings


# ==========================================================================
# THE SNAPSHOT SURFACE — `.rigor-effects.yml` (ADR-0043 § 3; issue #116)
# ==========================================================================
#
# The report comparison above grades a REPORT. This one grades the committed
# ARTIFACT, and it exists because the two disagree about which direction is
# safe.
#
# `Snapshot.build_methods` (`snapshot.rb:213-221`) drops a row through `omit?`
# (`:293-300`), and `omit?` READS THE TAINT BIT. ADR-0043 § 2 rules that being
# MORE tainted than the reference is sound — a non-exhaustive summary produces
# no finding — but a more-tainted row is a row `omit?` KEEPS, so extra taint
# does not under-claim here: it puts a symbol in a committed file that the
# oracle's file does not carry. § 2's own other rule calls that an OVER-CLAIM.
# `compare()` above scores exactly that shape `UNDER: extra-taint` and reports
# `OVER=0`; the class is structurally invisible to it. That is what this
# comparison is for.
#
# **It needs no port code.** The port ships no `effects update`, but every input
# `omit?` and `entry_for` take is already in `effects --full --format=json`
# (`effects`, `declared`, `exhaustive`, `causes`, `direct`), so the port's
# snapshot is SYNTHESISED here and graded against a real `rigor effects update`
# on the reference arm.
#
# ------------------------- THE NORMALISATIONS -----------------------------
# Every one of them, in one place, because a normalisation that quietly removes
# coverage is this repo's most expensive recurring bug (PR #115's root cause).
#
#  1. `rigor:` is excluded from the header comparison; the other three header
#     fields are compared. See `SNAPSHOT_HEADER_KEYS` for the per-field reason.
#  2. The port's `schema:` is the grader's constant `PORT_SNAPSHOT_SCHEMA`, its
#     `vocabulary:` is read from the port's vendored registry, and its
#     `config_digest:` is recomputed here. The port computes none of the three;
#     when `update` lands, all three move into it and this stops being a
#     stand-in. Stated so the next reader does not mistake agreement on these
#     lines for the port having produced them.
#  3. The compared artifacts are the DEFAULT tables, not `--full`. The default
#     table IS the committed file, and the omission rule is the thing under
#     test — grading `--full` would normalise `omit?` away entirely. (The
#     report comparison keeps `--full` for the opposite reason: there, omission
#     and a genuine empty summary are indistinguishable.)
#  4. `unresolved:` is compared and COUNTED but never fatal. 4,503 of
#     mastodon's 4,525 parameterised `dynamic-receiver(…)` causes carry a
#     reference-typer reason code (`unsupported_syntax`,
#     `inferred_return_untyped`) that a typer-free port cannot produce, so
#     grading it would be a gate that can only ever fail
#     (`docs/notes/20260826-effects-s5-probe.md` § 3).
#  5. `reach:` is UNGRADED for any project that configures a non-empty
#     `effects.snapshot.reach:`. The port has no transitive lane at all
#     (slice 4 DECLINED), so scoring those rows as under-claims would describe
#     a surface that does not exist. Every project in the standing set leaves
#     the key empty, where the reference itself writes `reach: {}` — so this is
#     a guard, not a behaviour.
#  6. A port JSON row missing `effects` or `exhaustive` is INVALID, not
#     defaulted. `lanes()`'s weakest-value rule ("a shape this tool does not
#     understand can only produce UNDER") CANNOT transfer to this surface:
#     defaulting `exhaustive` to False keeps the row and manufactures a
#     SNAPSHOT-OVER, defaulting it to True drops the row and hides one. There
#     is no safe default, so there is no default.
#  7. Row and label ordering is Python's `sorted()` over `str`, which equals
#     Ruby's byte-wise `sort` because UTF-8 preserves code-point order.
#
# The parser and the renderer below are checked on every run: the oracle's own
# file is parsed, re-rendered, and required to come back byte-identical before
# anything is graded (`ROUND-TRIP` in the output). `--self-test` adds the
# stronger check — `omit?` is re-derived over the oracle's OWN `--full`
# snapshot and must reproduce the oracle's default file exactly.
# --------------------------------------------------------------------------

# `Snapshot::SCHEMA` (`snapshot.rb:41`) and `Snapshot::HEADER` (`:43`).
PORT_SNAPSHOT_SCHEMA = 1
SNAPSHOT_COMMENT = ("# .rigor-effects.yml — generated by `rigor effects update`. "
                    "Commit it; review its diff.")
# `Summary::TRIVIAL_BOUND` (`summary.rb:37`) and `Snapshot::SYNTHESISED_ORIGINS`
# (`snapshot.rb:286`).
TRIVIAL_BOUND = ("mutate.local",)
SYNTHESISED_ORIGINS = {"construct:attr-writer"}
DEFAULT_SNAPSHOT_PATH = ".rigor-effects.yml"
PORT_REGISTRY = os.path.join(REPO, "crates", "rigor-effects", "vendor", "effects", "registry.yml")


class SnapshotError(Exception):
    """The snapshot comparison could not be made — never a verdict, always INVALID."""


# ---- the label algebra, transcribed (label.rb / label_set.rb) -------------
def _admits(bound_set, label):
    """`LabelSet#admits?` — segment-aware prefix subsumption, not string prefix.

    `io` admits `io.net.http` and does NOT admit `iota` (`label.rb:39-44`).
    """
    return any(label == bound or label.startswith(f"{bound}.") for bound in bound_set)


def _subsumed_by(labels, bound_set):
    return all(_admits(bound_set, label) for label in labels)


def _excluding_subsumed_by(labels, other):
    """`LabelSet#excluding_subsumed_by` — the declared lane's RENDERING rule."""
    return [label for label in labels if not _admits(other, label)]


def render_causes(causes):
    """`Snapshot.render_causes` (`snapshot.rb:276-279`): `cause` or `cause(detail)`,
    then `uniq.sort`. `causes` arrives as the JSON `[[cause, detail], …]` pairs."""
    out = set()
    for pair in causes:
        if not isinstance(pair, list) or not pair:
            raise SnapshotError(f"cause is not a [cause, detail] pair: {pair!r}")
        cause = pair[0]
        detail = pair[1] if len(pair) > 1 else None
        out.add(cause if not detail else f"{cause}({detail})")
    return sorted(out)


def omit(trivial, exhaustive, bundles, proven, declared):
    """`Snapshot.omit?` (`snapshot.rb:293-300`), transcribed clause for clause.

    `declared` is the RAW direct lane, as upstream passes it (`:216`); `trivial`
    is `Summary#trivial?`, which uses the RENDERED one. The third clause is the
    one that inverts ADR-0043 § 2: a row the port taints and the oracle does not
    survives `return false unless direct.exhaustive?` and is WRITTEN.
    """
    if trivial:
        return True
    if not proven and not declared:
        return True
    if not exhaustive:
        return False
    if not bundles:
        return False
    return all(origin in SYNTHESISED_ORIGINS for origin in bundles)


# ---- the serialiser, transcribed (snapshot.rb:338-377) --------------------
def _scalar(value):
    """`Snapshot#scalar` — `value.to_s.to_json`. Ruby's `String#to_json` leaves
    non-ASCII as UTF-8, so `ensure_ascii=False` is the matching spelling."""
    return json.dumps(str(value), ensure_ascii=False)


def _wire(value):
    """`Snapshot#wire` — flow sequences `", "`-joined, everything else `to_s`."""
    if isinstance(value, list):
        return "[" + ", ".join(_scalar(member) for member in value) + "]"
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def _row_wire_fields(row):
    """`Entry#to_h` (`snapshot.rb:59-66`) — the field order, and the three
    omissions a reader defaults: `declared:` when empty, `exhaustive:` when
    true, `unresolved:` when empty."""
    yield "effects", row["effects"]
    if row["declared"]:
        yield "declared", row["declared"]
    if not row["exhaustive"]:
        yield "exhaustive", False
    if row["unresolved"]:
        yield "unresolved", row["unresolved"]


def render_snapshot(header, methods, reach):
    """`Snapshot#to_yaml` — a hand-rolled JSON-compatible YAML subset, so the
    leading comment, the fixed key order and the flow sequences are upstream's
    own rather than `YAML.dump`'s."""
    lines = [SNAPSHOT_COMMENT,
             f"schema: {header['schema']}",
             f"rigor: {_scalar(header['rigor'])}",
             f"vocabulary: {header['vocabulary']}",
             f"config_digest: {_scalar(header['config_digest'])}"]
    return "\n".join(lines) + "\n" + render_tables(methods, reach)


def render_tables(methods, reach):
    """The BODY — everything below the four header lines. Compared on its own,
    because that is the half neither implementation is allowed to differ on."""
    lines = []
    for name, table in (("methods", methods), ("reach", reach)):
        if not table:
            lines.append(f"{name}: {{}}")
            continue
        lines.append(f"{name}:")
        for key in sorted(table):
            lines.append(f"  {_scalar(key)}:")
            for field, value in _row_wire_fields(table[key]):
                lines.append(f"    {field}: {_wire(value)}")
    return "\n".join(lines) + "\n"


def _wire_in(text):
    if text.startswith("["):
        return json.loads(text)
    if text in ("true", "false"):
        return text == "true"
    if text.startswith('"'):
        return json.loads(text)
    return int(text)


def parse_snapshot(text):
    """Read the reference's own `.rigor-effects.yml` back into (header, methods,
    reach), with `Entry.from_h`'s defaults applied.

    Hand-written rather than `yaml.safe_load`ed on purpose: the file is a
    DECLARED subset (`snapshot.rb:36-38`), so a line this cannot read is a shape
    surprise worth failing on rather than a value worth guessing. It also keeps
    a fixture-only run free of PyYAML for everything but `config_digest`.
    """
    header, tables, section, key = {}, {"methods": {}, "reach": {}}, None, None
    for line in text.split("\n"):
        if not line or line.startswith("#"):
            continue
        if line.startswith("    "):
            if section is None or key is None:
                raise SnapshotError(f"field outside a row: {line!r}")
            field, _, value = line[4:].partition(":")
            if field not in ("effects", "declared", "exhaustive", "unresolved"):
                raise SnapshotError(f"unknown row field {field!r}")
            tables[section][key][field] = _wire_in(value.strip())
        elif line.startswith("  "):
            if section is None:
                raise SnapshotError(f"row outside a table: {line!r}")
            key = json.loads(line[2:].rstrip().rstrip(":"))
            tables[section][key] = {}
        else:
            name, _, value = line.partition(":")
            if name in ("methods", "reach"):
                section, key = name, None
                if value.strip() not in ("", "{}"):
                    raise SnapshotError(f"unexpected table opener: {line!r}")
            elif name in ("schema", "rigor", "vocabulary", "config_digest"):
                header[name] = _wire_in(value.strip())
            else:
                raise SnapshotError(f"unknown top-level key {name!r}")
    if set(header) != {"schema", "rigor", "vocabulary", "config_digest"}:
        raise SnapshotError(f"header fields are {sorted(header)}")
    for table in tables.values():
        for row in table.values():
            row.setdefault("effects", [])
            row.setdefault("declared", [])
            row.setdefault("exhaustive", True)
            row.setdefault("unresolved", [])
    return header, tables["methods"], tables["reach"]


# ---- the port's side, synthesised from the JSON it already emits ----------
def synthesise_port_methods(json_methods):
    """The `methods:` table the port WOULD write, from `effects --full --format=json`.

    `build_methods` records `entry.direct` — the collector's own-body summary
    (`snapshot.rb:213-221`), which is exactly what the port computes: its
    `effects` / `exhaustive` / `causes` are direct readings, because the
    transitive lane was declined, and `direct` is the per-origin bundle map.
    """
    out = {}
    for key, entry in json_methods.items():
        if not isinstance(entry, dict) or "effects" not in entry or "exhaustive" not in entry:
            # Normalisation 6: there is no safe default for a missing lane here.
            raise SnapshotError(f"{key}: JSON row lacks `effects` or `exhaustive`")
        proven = sorted(set(entry["effects"]))
        declared = sorted(set(entry.get("declared") or []))
        exhaustive = bool(entry["exhaustive"])
        bundles = entry.get("direct") or {}
        rendered = _excluding_subsumed_by(declared, proven)
        trivial = exhaustive and _subsumed_by(proven, TRIVIAL_BOUND) and not rendered
        if omit(trivial=trivial, exhaustive=exhaustive, bundles=bundles,
                proven=proven, declared=declared):
            continue
        out[key] = {"effects": proven, "declared": rendered, "exhaustive": exhaustive,
                    "unresolved": [] if exhaustive else render_causes(entry.get("causes") or [])}
    return out


def port_snapshot_header(project, rs_version):
    """The header the port's `update` would write, assembled from PORT-SIDE facts.

    `schema:` is the grader's constant, `vocabulary:` is the port's own vendored
    registry, `config_digest:` is recomputed from the project's config. See
    normalisation 2: agreement on these three lines is agreement with the
    grader's stand-in, not with a writer the port has.
    """
    return {"schema": PORT_SNAPSHOT_SCHEMA, "rigor": rs_version,
            "vocabulary": port_vocabulary(),
            "config_digest": config_digest(project_effects(project))}


def port_vocabulary():
    import yaml  # local, matching `real_targets`' idiom

    with open(PORT_REGISTRY, encoding="utf-8") as handle:
        return yaml.safe_load(handle).get("vocabulary", 0)


def project_effects(project):
    """The project's `effects:` block as `Configuration#effects` holds it.

    `coerce_effects` (`configuration.rb:683-689`) keeps the RAW parsed hash, and
    answers `nil` for an absent key — which `config_digest` then reads as `{}`,
    the same value `rigor effects` supplies for its own run. A non-Hash body is
    `{}` too.
    """
    path = os.path.join(project, ".rigor.yml")
    if not os.path.isfile(path):
        return {}
    import yaml

    with open(path, encoding="utf-8") as handle:
        data = yaml.safe_load(handle) or {}
    block = data.get("effects")
    return block if isinstance(block, dict) else {}


def _canonicalize(value):
    """`Identity.canonicalize` — keys stringified and sorted at every depth."""
    if isinstance(value, dict):
        pairs = sorted(value.items(), key=lambda kv: str(kv[0]))
        return {str(key): _canonicalize(member) for key, member in pairs}
    if isinstance(value, list):
        return [_canonicalize(member) for member in value]
    return value


def config_digest(effects_block):
    """`Snapshot.config_digest` / `Identity.config_digest` (`identity.rb:90-92`):
    `SHA256(JSON.generate(canonicalize(configuration.effects || {})))`.

    Recomputed here rather than read from either engine. What it catches: a pin
    that changes the recipe, and a project whose `effects:` block the reference
    resolved differently from the file on disk (a merged `.rigor.dist.yml`, an
    ancestor config). What it does NOT catch: a port that computes the digest
    wrongly — the port has no digest, and when it grows one this becomes a real
    two-way comparison rather than a one-way reproduction.
    """
    # `JSON.generate` is compact and leaves non-ASCII as UTF-8; `separators` and
    # `ensure_ascii=False` are the matching spelling.
    wire = json.dumps(_canonicalize(effects_block), separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(wire.encode("utf-8")).hexdigest()


def project_reach_globs(project):
    """`effects.snapshot.reach:`, which defaults to `[]` (`configuration.rb:718`)
    and makes `build_reach` return `{}` (`snapshot.rb:228`). Normalisation 5."""
    snapshot = project_effects(project).get("snapshot")
    reach = snapshot.get("reach") if isinstance(snapshot, dict) else None
    return list(reach) if isinstance(reach, list) else []


def project_snapshot_path(project):
    snapshot = project_effects(project).get("snapshot")
    path = snapshot.get("path") if isinstance(snapshot, dict) else None
    return str(path) if path else DEFAULT_SNAPSHOT_PATH


@contextlib.contextmanager
def _preserving(path):
    """Restore `path` to exactly its prior state — bytes, or absence.

    `rigor effects update` overwrites its target with no guard of any kind
    (probe § 5b), and for a FIXTURE project the target is a file in this repo's
    own working tree. `project_dir` makes a real corpus residue-proof by copying
    it; a fixture is measured in place, so the residue defence has to be here.
    """
    before = None
    if os.path.exists(path):
        with open(path, "rb") as handle:
            before = handle.read()
    try:
        yield
    finally:
        if before is None:
            with contextlib.suppress(OSError):
                os.remove(path)
        else:
            with open(path, "wb") as handle:
                handle.write(before)


def run_ref_update(project, full=False):
    """`rigor effects update` on the reference arm, and the file it wrote.

    Same load-path pinning and same `.rigor/cache` discipline as `run_ref`. The
    target is preserved: this command's whole job is to overwrite it.
    """
    ref_plugin = os.path.join(REF_DIR, "plugins", "rigor-rbs-inline", "lib")
    target = os.path.join(project, project_snapshot_path(project))
    argv = ["ruby", "-I", REF_LIB, "-I", ref_plugin, REF_EXE, "effects", "update"]
    if full:
        argv.append("--full")
    with _preserving(target):
        _clear_ref_cache(project)
        try:
            result = subprocess.run(argv, capture_output=True, text=True, cwd=project,
                                    stdin=subprocess.DEVNULL)
        finally:
            _clear_ref_cache(project)
        if result.returncode != 0 or not os.path.isfile(target):
            raise SnapshotError(f"`effects update` exit {result.returncode}: "
                                f"{(result.stderr or '').strip().splitlines()[-1:]}")
        with open(target, encoding="utf-8") as handle:
            return handle.read()


def compare_snapshots(ref, rs):
    """Per-row verdicts over the two `methods:` tables.

    SNAPSHOT-OVER is the verdict `compare()` cannot express: a row the oracle's
    RECORD does not carry. It is reached by an extra proven label, by claimed
    exhaustiveness — and, uniquely here, by the port merely being MORE TAINTED,
    which `omit?` turns from an under-claim into a manufactured symbol.
    """
    verdicts, findings = Counter(), []
    for name in sorted(set(ref) | set(rs)):
        if name not in rs:
            verdicts["UNDER"] += 1
            verdicts["under:absent-row"] += 1
            continue
        row = rs[name]
        if name not in ref:
            verdicts["OVER"] += 1
            verdicts["over:row-the-oracle-omits"] += 1
            findings.append(("SNAPSHOT-OVER", name,
                             "row the oracle's record does not carry — "
                             f"effects={row['effects']} exhaustive={row['exhaustive']}"))
            continue
        gold = ref[name]
        over_labels = set(row["effects"]) - set(gold["effects"])
        under_labels = set(gold["effects"]) - set(row["effects"])
        fatal = False
        if over_labels:
            verdicts["OVER"] += 1
            verdicts["over:extra-label"] += 1
            fatal = True
            findings.append(("SNAPSHOT-OVER", name,
                             f"direct labels the oracle's record does not carry: "
                             f"{sorted(over_labels)}"))
        if row["exhaustive"] and not gold["exhaustive"]:
            verdicts["OVER"] += 1
            verdicts["over:claims-exhaustiveness"] += 1
            fatal = True
            findings.append(("SNAPSHOT-OVER", name,
                             "claims exhaustiveness the oracle's record does not"))
        if row["declared"] != gold["declared"]:
            verdicts["DECLARED-MISMATCH"] += 1
            fatal = True
            findings.append(("SNAPSHOT-DECLARED-MISMATCH", name,
                             f"declared {row['declared']} != oracle {gold['declared']}"))
        if fatal:
            continue
        if under_labels:
            verdicts["UNDER"] += 1
            verdicts["under:missing-label"] += 1
        elif gold["exhaustive"] and not row["exhaustive"]:
            verdicts["UNDER"] += 1
            verdicts["under:extra-taint"] += 1
        elif row["unresolved"] != gold["unresolved"]:
            # Normalisation 4: counted, never fatal, never MATCH.
            verdicts["UNRESOLVED-ONLY"] += 1
        else:
            verdicts["MATCH"] += 1
    return verdicts, findings


def compare_snapshot_headers(ref_header, rs_header):
    findings = []
    for field in sorted(set(ref_header) | set(rs_header)):
        if field in SNAPSHOT_HEADER_KEYS:
            continue
        if ref_header.get(field) != rs_header.get(field):
            findings.append(("SNAPSHOT-HEADER", field,
                             f"port {rs_header.get(field)!r} != oracle {ref_header.get(field)!r}"))
    return findings


def measure_snapshot(project, label, rs_methods, rs_version, show):
    """The snapshot half of one project's measurement. Returns a Counter, or
    None when the comparison could not be made (INVALID, never delta-free)."""
    print(f"  --- snapshot ({project_snapshot_path(project)}) ---")
    try:
        text = run_ref_update(project)
        ref_header, ref_methods, ref_reach = parse_snapshot(text)
        if render_snapshot(ref_header, ref_methods, ref_reach) != text:
            raise SnapshotError("ROUND-TRIP FAILED — this tool's parser/renderer does not "
                                "reproduce the oracle's own file; every verdict below would be "
                                "noise")
        rs_header = port_snapshot_header(project, rs_version)
        rs_snapshot = synthesise_port_methods(rs_methods)
    except SnapshotError as error:
        print(f"      INVALID: {error}")
        return None
    globs = project_reach_globs(project)
    verdicts, findings = compare_snapshots(ref_methods, rs_snapshot)
    header_findings = compare_snapshot_headers(ref_header, rs_header)
    verdicts["HEADER-MISMATCH"] += len(header_findings)
    findings = header_findings + findings
    # The port has no `reach:` lane, so its body always carries `reach: {}` —
    # which is the file the reference itself writes for every project that has
    # not opted in (normalisation 5 guards the projects that have).
    body_ref = render_tables(ref_methods, ref_reach)
    body_rs = render_tables(rs_snapshot, {})
    print(f"      oracle={len(ref_methods)} rows   port={len(rs_snapshot)} rows   "
          f"ROUND-TRIP ok   body byte-identical: {'yes' if body_ref == body_rs else 'no'}")
    print(f"      MATCH={verdicts['MATCH']}  UNDER={verdicts['UNDER']}  "
          f"OVER={verdicts['OVER']}  DECLARED-MISMATCH={verdicts['DECLARED-MISMATCH']}  "
          f"HEADER-MISMATCH={verdicts['HEADER-MISMATCH']}  "
          f"unresolved-only={verdicts['UNRESOLVED-ONLY']}")
    if verdicts["UNDER"]:
        detail = {k.split(":", 1)[1]: v for k, v in verdicts.items() if k.startswith("under:")}
        print(f"      UNDER by kind: {detail}")
    if globs:
        # Normalisation 5.
        print(f"      reach: UNGRADED — the project configures {globs}, a table the port "
              f"has no lane for (oracle carries {len(ref_reach)} reach rows)")
    for kind, name, why in findings[:show]:
        print(f"        {kind}: {name} — {why}")
    if len(findings) > show:
        print(f"        … {len(findings) - show} more not listed (raise --show)")
    return verdicts


def snapshot_controls():
    """The must-fire controls for everything above that has no oracle to check it.

    `self_test_snapshot` proves the omission rule and the serialiser against a
    real reference file, but nothing there can prove that a verdict FIRES: the
    oracle agrees with itself, so a `compare_snapshots` that returned MATCH
    unconditionally would pass it. These are that half — the direction this repo
    has been burned by four times ("we do strictly less" has failed on the wrong
    axis, the wrong carrier and inverted), and the reason a widening always owes
    a control that still fires.

    Pure functions only, so this costs nothing and runs before any subprocess.
    """
    row = lambda eff, ex=True, un=(), de=(): {  # noqa: E731 — a table, not a function
        "effects": list(eff), "declared": list(de), "exhaustive": ex, "unresolved": list(un)}
    header = {"schema": 1, "rigor": "0.3.4", "vocabulary": 1, "config_digest": "d"}
    port_header = {**header, "rigor": "0.0.1"}

    def verdicts(ref, rs):
        return compare_snapshots(ref, rs)[0]

    round_trip = render_snapshot(header, {'A#a"q': row(["io"], ex=False, un=["dynamic-send"],
                                                       de=["io.db"])}, {})
    checks = [
        # compare_snapshots — one per verdict, in both directions where there are two.
        ("an identical row is MATCH",
         verdicts({"A#a": row(["io.fs.read"])}, {"A#a": row(["io.fs.read"])})["MATCH"] == 1),
        ("a row the oracle's record omits is SNAPSHOT-OVER",
         verdicts({}, {"A#a": row(["io.fs.read"])})["over:row-the-oracle-omits"] == 1),
        ("an extra direct label is SNAPSHOT-OVER",
         verdicts({"A#a": row([])}, {"A#a": row(["io.fs.read"])})["over:extra-label"] == 1),
        ("claimed exhaustiveness is SNAPSHOT-OVER",
         verdicts({"A#a": row([], ex=False, un=["dynamic-send"])},
                  {"A#a": row([])})["over:claims-exhaustiveness"] == 1),
        ("a missing label is UNDER and not fatal",
         verdicts({"A#a": row(["io"])}, {"A#a": row([], ex=False, un=["x"])})["OVER"] == 0),
        ("extra taint on a SHARED row is UNDER",
         verdicts({"A#a": row(["io"])}, {"A#a": row(["io"], ex=False, un=["x"])})
         ["under:extra-taint"] == 1),
        ("an absent row is UNDER",
         verdicts({"A#a": row(["io"])}, {})["under:absent-row"] == 1),
        ("an `unresolved:`-only difference is neither MATCH nor fatal",
         verdicts({"A#a": row([], ex=False, un=["a"])}, {"A#a": row([], ex=False, un=["b"])})
         == Counter({"UNRESOLVED-ONLY": 1})),
        ("a declared-lane difference is fatal",
         verdicts({"A#a": row(["io"], de=["io.db"])}, {"A#a": row(["io"])})
         ["DECLARED-MISMATCH"] == 1),
        # The header, now that three of its four keys are compared.
        ("the version line alone never fires",
         compare_snapshot_headers(header, port_header) == []),
        ("a `schema:` bump fires",
         len(compare_snapshot_headers({**header, "schema": 2}, port_header)) == 1),
        ("a `vocabulary:` bump fires",
         len(compare_snapshot_headers({**header, "vocabulary": 2}, port_header)) == 1),
        ("a `config_digest:` difference fires",
         len(compare_snapshot_headers({**header, "config_digest": "e"}, port_header)) == 1),
        # omit?, clause by clause, including the one issue #116 is about.
        ("a trivial row is omitted",
         omit(True, True, {"construct:ivar-write"}, ["mutate.local"], [])),
        ("a taint-only row is omitted", omit(False, False, {}, [], [])),
        ("a TAINTED `mutate.local` row is KEPT — the § 5a inversion, in one line",
         not omit(False, False, {"construct:receiver-mutation"}, ["mutate.local"], [])),
        ("a synthesised attr-writer row is omitted",
         omit(False, True, {"construct:attr-writer"}, ["mutate.self"], [])),
        ("a hand-written writer keeps its row",
         not omit(False, True, {"construct:ivar-write"}, ["mutate.self"], [])),
        # The label algebra is segment-aware, not a string prefix.
        ("`io` admits `io.net.http`", _admits(["io"], "io.net.http")),
        ("`io` does NOT admit `iota`", not _admits(["io"], "iota")),
        ("`mutate.self` is outside the trivial bound",
         not _subsumed_by(["mutate.self"], TRIVIAL_BOUND)),
        # The serialiser's three omissions and its JSON scalars.
        ("`declared:` is omitted when empty",
         "declared" not in render_tables({"A#a": row(["io"])}, {})),
        ("`exhaustive:` is omitted when true",
         "exhaustive" not in render_tables({"A#a": row(["io"])}, {})),
        ("an empty table renders as `{}`", "reach: {}" in render_tables({"A#a": row(["io"])}, {})),
        ("a key carrying a quote is JSON-escaped", '"A#a\\"q"' in round_trip),
        ("parse ∘ render is the identity", render_snapshot(*parse_snapshot(round_trip))
         == round_trip),
        # The digest recipe, against the value the reference itself writes for a
        # project with no `effects:` block.
        ("an absent `effects:` block digests as sha256('{}')", config_digest({}) ==
         "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"),
    ]
    try:
        synthesise_port_methods({"A#a": {"effects": []}})
        checks.append(("a port JSON row with no `exhaustive` is INVALID, not defaulted", False))
    except SnapshotError:
        checks.append(("a port JSON row with no `exhaustive` is INVALID, not defaulted", True))
    failed = [name for name, ok in checks if not ok]
    print(f"\n=== SNAPSHOT CONTROLS ({len(checks)}) ===")
    for name in failed:
        print(f"  FAIL: {name}")
    print(f"  {'BROKEN — a verdict that cannot fire is not a verdict.' if failed else 'PASS'}")
    return not failed


def self_test_snapshot(project, label):
    """Re-derive the oracle's DEFAULT snapshot from the oracle's OWN `--full` one.

    The instrument's gate for this surface, and the reason a disagreement below
    can be read as the port's rather than as this file's. `update --full` writes
    a row for every entry carrying `direct.proven`, the rendered direct declared
    lane, `direct.exhaustive?` and the rendered direct causes; the JSON's
    `direct` key supplies the per-origin bundles `omit?`'s last two clauses
    need. Apply `omit?` to that and the result must be the oracle's default file
    BYTE FOR BYTE — header included.

    One shortcut, and it is exact rather than approximate: `omit?`'s second
    clause reads the RAW declared lane where the full file carries the RENDERED
    one, and the two coincide whenever `proven` is empty — which is the only
    case that clause can reach.
    """
    print(f"  --- snapshot re-derivation ({label}) ---")
    try:
        default_text = run_ref_update(project)
        full_text = run_ref_update(project, full=True)
        header, full_methods, full_reach = parse_snapshot(full_text)
        if render_snapshot(header, full_methods, full_reach) != full_text:
            raise SnapshotError("round-trip of the oracle's --full file failed")
        bundles = run_ref(project)
        if bundles is None:
            raise SnapshotError("the reference produced no parseable effects JSON")
        rebuilt = {}
        for key, row in full_methods.items():
            if key not in bundles:
                raise SnapshotError(f"{key} is in the --full snapshot but not in the "
                                    "--full report")
            direct = bundles[key].get("direct") or {}
            trivial = (row["exhaustive"] and _subsumed_by(row["effects"], TRIVIAL_BOUND)
                       and not row["declared"])
            if omit(trivial=trivial, exhaustive=row["exhaustive"], bundles=direct,
                    proven=row["effects"], declared=row["declared"]):
                continue
            rebuilt[key] = row
        rederived = render_snapshot(header, rebuilt, full_reach)
    except SnapshotError as error:
        print(f"      INVALID: {error}")
        return False
    ok = rederived == default_text
    print(f"      --full rows={len(full_methods)}  re-derived default rows={len(rebuilt)}  "
          f"oracle default rows={len(parse_snapshot(default_text)[1])}")
    print("      " + ("PASS — `omit?` and the serialiser reproduce the oracle's own file byte "
                      "for byte." if ok else
                      "BROKEN: the re-derived default snapshot is not the oracle's."))
    if not ok:
        import difflib
        for line in list(difflib.unified_diff(default_text.split("\n"), rederived.split("\n"),
                                              "oracle", "re-derived", lineterm=""))[:40]:
            print(f"        {line}")
    return ok


# --------------------------------------------------------------------------
# The unit of measurement: a PROJECT DIRECTORY.
#
# A fixture project already is one. A real corpus is a plain checkout — no
# `.rigor.yml`, so `rigor effects` has no project to analyse and no closed world
# to scope. Making one has exactly two honest shapes, and this tool takes the
# second:
#
#   1. synthesise a `.rigor.yml` in a temp cwd beside a SYMLINK to the real tree
#      — O(1), and measured to give bit-identical results (2026-08-26);
#   2. COPY the tree into a temp project — ~0.6 s for mastodon/app's 38 MB,
#      ~1.4 s for gitlab-foss/lib.
#
# The copy wins on the constraint that outranks run time: **the user's checkout
# must not be able to receive residue**, and a copy makes that structural rather
# than conditional. A symlink leaves a live writable path into the checkout for
# the duration of the run — today's writes (`.rigor/cache`, `.rigor-effects.yml`)
# land in the cwd, but nothing about the arrangement PREVENTS a future write
# beside a source file, and stray `.rigor.yml` files in these very checkouts have
# had to be cleaned up by hand once already.
#
# The copy also closes an ambush the symlink leaves open: config discovery. The
# temp project has no ancestor directory to walk into, whereas the real
# `gitlab-foss/lib` sits one level under a REAL `.rigor.yml` with its own
# `paths:`, `plugins:` and `severity_profile:`. Neither engine walks up into it
# today — measured equal on both arms — but "measured equal today" is what the
# pin bumps for a living, and a confound in the instrument is worth more than
# 0.6 s.
# --------------------------------------------------------------------------
SYNTHETIC_CONFIG = """\
# SYNTHESISED by harness/effects_diff.py — not part of the corpus checkout.
#
# `rigor effects` needs a project; a sweep corpus is a plain tree. This config
# and the copy of that tree beside it are created in a temp directory per run
# and removed afterwards, so the checkout itself is only ever READ. Deliberately
# minimal: no `plugins:`, no `severity_profile:`, so the two arms differ in
# nothing but the engine under test.
paths:
  - {name}
"""


@contextlib.contextmanager
def project_dir(target):
    """The directory the two arms run in, for one target.

    A fixture yields its own path. A real corpus yields a temp project holding a
    copy of the tree and a synthesised config, removed on the way out — on the
    exception path too, which is why this is a context manager and not two calls.
    """
    if target["kind"] == "fixture":
        yield target["path"]
        return
    parent = tempfile.mkdtemp(prefix="rigor-effects-")
    try:
        name = os.path.basename(target["path"].rstrip(os.sep))
        # `symlinks=True`: a symlink inside the corpus is copied AS a link rather
        # than followed, so neither a link cycle nor a link pointing at half the
        # user's disk can turn the copy into a runaway.
        shutil.copytree(target["path"], os.path.join(parent, name), symlinks=True)
        with open(os.path.join(parent, ".rigor.yml"), "w", encoding="utf-8") as handle:
            handle.write(SYNTHETIC_CONFIG.format(name=name))
        yield parent
    finally:
        shutil.rmtree(parent, ignore_errors=True)


def self_test(target, show):
    """Grade the reference against ITSELF: every method must be MATCH.

    The comparison's own gate. A `compare()` that returned MATCH unconditionally,
    or one that mis-read a lane so that every method looked like an over-claim,
    would be invisible while the port reports nothing — the whole first slice
    runs with one arm empty, which is exactly when a broken instrument reads as a
    green one.

    Real projects are included: a self-diff over one also proves the temp-project
    mechanism produces a project the reference can actually analyse, which an
    all-fixture self-test cannot.
    """
    label = target["label"]
    print(f"\n=== SELF-TEST {label} ===")
    with project_dir(target) as project:
        ref = run_ref(project)
        snapshot_ok = None if target.get("no_snapshot") else self_test_snapshot(project, label)
    if ref is None:
        print("  INVALID: the reference produced no parseable effects JSON.")
        return None
    if snapshot_ok is False:
        print("  BROKEN: the snapshot re-derivation does not reproduce the oracle's own file.")
        return None
    verdicts, findings = compare(ref, ref)
    print(f"  oracle={len(ref)} methods   MATCH={verdicts['MATCH']}  "
          f"UNDER={verdicts['UNDER']}  OVER={verdicts['OVER']}  "
          f"DECLARED-MISMATCH={verdicts['DECLARED-MISMATCH']}")
    if verdicts["MATCH"] != len(ref) or verdicts["UNDER"] or verdicts["OVER"]:
        print("  BROKEN: a self-diff must be all MATCH.")
        for kind, name, why in findings[:show]:
            print(f"    {kind}: {name} — {why}")
    return verdicts


def measure(rs_bin, target, show, snapshot=True, rs_version="?"):
    """One project's two comparisons. Returns `(report_verdicts, snapshot_verdicts)`,
    either of which is None when that comparison was INVALID.

    The two are kept apart all the way to the exit code: the report comparison is
    the standing gate with recorded per-project verdicts, and a snapshot number
    moving must never be able to read as one of those moving.
    """
    label = target["label"]
    t = time.time()
    with project_dir(target) as project:
        ref = run_ref(project)
        rs = run_rs(rs_bin, project)
        print(f"\n=== {label} ({time.time() - t:.1f}s) ===")
        if ref is None:
            print("  INVALID: the reference produced no parseable effects JSON "
                  "— comparison invalid, not delta-free.")
            return None, None
        if rs is None:
            print(f"  INVALID: rigor-rs failed [{LAST_RS_FAILURE}] "
                  "— comparison invalid, not delta-free.")
            return None, None
        if LAST_RS_FAILURE == "NOT-IMPLEMENTED":
            print("  rigor-rs: `effects` NOT IMPLEMENTED — every oracle method counts as UNDER.")
        verdicts, findings = compare(ref, rs)
        ref_labels = sum(len(lanes(e)[0]) for e in ref.values())
        rs_labels = sum(len(lanes(e)[0]) for e in rs.values())
        print(f"  oracle={len(ref)} methods / {ref_labels} proven labels   "
              f"rigor-rs={len(rs)} / {rs_labels}")
        print(f"  MATCH={verdicts['MATCH']}  UNDER={verdicts['UNDER']}  "
              f"OVER={verdicts['OVER']}  DECLARED-MISMATCH={verdicts['DECLARED-MISMATCH']}")
        if verdicts["UNDER"]:
            detail = {k.split(":", 1)[1]: v for k, v in verdicts.items() if k.startswith("under:")}
            print(f"  UNDER by kind: {detail}")
        for kind, name, why in findings[:show]:
            print(f"    {kind}: {name} — {why}")
        if len(findings) > show:
            print(f"    … {len(findings) - show} more not listed (raise --show)")
        # The snapshot runs LAST, after both report arms: `effects update`
        # writes into the project, and nothing measured may see that file.
        snap = measure_snapshot(project, label, rs, rs_version, show) if snapshot else None
    return verdicts, snap


def _fixture(path):
    label = os.path.relpath(path, REPO) if path.startswith(REPO) else path
    return {"kind": "fixture", "label": label, "path": path}


def fixture_targets():
    if not os.path.isdir(DEFAULT_CORPUS):
        return []
    return [_fixture(os.path.join(DEFAULT_CORPUS, d))
            for d in sorted(os.listdir(DEFAULT_CORPUS))
            if os.path.isdir(os.path.join(DEFAULT_CORPUS, d))]


def real_targets(scale):
    """The real projects of the standing set, split into present and absent.

    Absent ones are RETURNED, not dropped — the caller reports them, because a
    run that quietly measured half its set reads as a green gate
    (`fp_audit.py`'s contract, and the reason `sweep-corpora.yml` says so in its
    header).
    """
    import yaml  # local: a bare fixture run has no dependency on PyYAML

    with open(SWEEP_MANIFEST, encoding="utf-8") as handle:
        paths = {e["label"]: e["path"] for e in yaml.safe_load(handle).get("corpora", [])}
    present, absent = [], []
    for entry in REAL_PROJECTS:
        if entry["opt_in"] and not scale:
            continue
        label = entry["label"]
        if label not in paths:
            # Not a machine fact: this table names a corpus the repo's own
            # membership list no longer carries. Fail rather than skip.
            sys.exit(f"ERROR: REAL_PROJECTS names `{label}`, which is not in "
                     f"{SWEEP_MANIFEST}.\n"
                     "       The manifest is the single membership list; fix one or the other.")
        target = {"kind": "real", "label": label, "path": paths[label],
                  "opt_in": entry["opt_in"]}
        (present if os.path.isdir(paths[label]) else absent).append(target)
    return present, absent


def resolve_targets(args, scale):
    """Everything to measure, plus the real corpora this machine does not have.

    Positional arguments REPLACE the whole standing set — the same rule
    `run_corpus.rb` states for its corpus list — so an ad-hoc project run costs
    nothing and reports no skips.
    """
    if args:
        return [_fixture(os.path.abspath(a)) for a in args], []
    present, absent = real_targets(scale)
    return fixture_targets() + present, absent


def report_skips(absent):
    for target in absent:
        print(f"\n=== {target['label']} — SKIPPED: {target['path']} is not on this "
              "machine (the standing set is INCOMPLETE for this run) ===")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("projects", nargs="*")
    ap.add_argument("--show", type=int, default=20)
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--self-test", action="store_true",
                    help="grade the reference against itself; every method must MATCH")
    ap.add_argument("--scale", action="store_true",
                    help="also measure the opt-in large real corpora (~40 s each)")
    ap.add_argument("--no-snapshot", action="store_true",
                    help="skip the `.rigor-effects.yml` comparison (one oracle run per project)")
    ap.add_argument("--snapshot-gate", action="store_true",
                    help="let the snapshot comparison set the exit code (see SNAPSHOT RESULT)")
    args = ap.parse_args()

    targets, absent = resolve_targets(args.projects, args.scale)
    if args.list:
        for t in targets:
            print(f"{t['kind']:8} {t['label']:34} {t['path']}")
        for t in absent:
            print(f"{'ABSENT':8} {t['label']:34} {t['path']}")
        return 0
    if args.self_test:
        print(f"reference:       {REF_DIR}")
        broken = 0 if snapshot_controls() else 1
        for t in targets:
            v = self_test(t, args.show)
            if v is None or v["OVER"] or v["UNDER"] or v["DECLARED-MISMATCH"]:
                broken += 1
        report_skips(absent)
        print("\nRESULT: " + ("FAIL — the comparison itself is broken."
                              if broken else "PASS — the comparison is sound on every project."
                                             + _incomplete(absent)))
        return 1 if broken else 0
    if not targets:
        sys.exit(f"ERROR: no projects to measure (default corpus {DEFAULT_CORPUS} is absent).\n"
                 "       A project is a directory with a `.rigor.yml` naming its `paths:`.")

    rs_bin = resolve_rs_binary()
    print(f"reference:       {REF_DIR}")
    snapshot = not args.no_snapshot
    rs_version = _rs_version(rs_bin)
    if snapshot:
        print(f"port snapshot header: schema={PORT_SNAPSHOT_SCHEMA}  "
              f"vocabulary={port_vocabulary()} (vendored)  rigor={rs_version!r} (EXCLUDED)")

    totals, snap_totals = Counter(), Counter()
    invalid, snap_invalid = [], []
    for t in targets:
        v, snap = measure(rs_bin, t, args.show, snapshot=snapshot, rs_version=rs_version)
        if v is None:
            invalid.append(t)
            if snapshot:
                snap_invalid.append(t)
            continue
        totals.update(v)
        if snapshot and snap is None:
            snap_invalid.append(t)
        elif snapshot:
            snap_totals.update(snap)
    report_skips(absent)

    print(f"\nTOTAL  MATCH={totals['MATCH']}  UNDER={totals['UNDER']}  "
          f"OVER={totals['OVER']}  DECLARED-MISMATCH={totals['DECLARED-MISMATCH']}")
    if invalid:
        print(f"INVALID comparisons: {len(invalid)} — "
              f"{', '.join(t['label'] for t in invalid)}")
    fatal = totals["OVER"] + totals["DECLARED-MISMATCH"]
    result = 0
    if fatal or invalid:
        print("\nRESULT: FAIL — the port may never claim an effect the oracle does not prove.")
        result = 1
    else:
        # A skipped corpus does not fail the run — the standing set is machine-local
        # and a member may legitimately be absent — but it may not let the run read
        # as a clean pass either. `RESULT: PASS` is the line that gets grepped and
        # quoted into a note, so the incompleteness rides on THAT line and not only
        # on a SKIPPED banner further up.
        print("\nRESULT: PASS — no over-claims. (UNDER is the arc's odometer, not a failure.)"
              + _incomplete(absent))
    if snapshot:
        result = _snapshot_result(snap_totals, snap_invalid, args.snapshot_gate) or result
    return result


def _rs_version(rs_bin):
    """The port's own `--version`, for the `rigor:` header line it would write.

    Asked of the binary rather than read from `Cargo.toml`, so the synthesised
    file carries what the port would actually print. The line is EXCLUDED from
    the comparison; it is rendered so the artifact this tool builds is a real
    snapshot rather than one with a hole in it.
    """
    try:
        out = subprocess.run([rs_bin, "--version"], capture_output=True, text=True,
                             stdin=subprocess.DEVNULL).stdout
    except OSError:
        return "?"
    return out.strip().split()[-1] if out.strip() else "?"


def _snapshot_result(totals, invalid, gate):
    """The snapshot comparison's own verdict line, and whether it sets the exit code.

    **It does not gate by default, and that is a dated decision, not a posture.**
    The comparison FAILS TODAY (issue #116 / the slice-5 probe § 5a:
    `07_mutators`, two rows), and the fix is a design question — does the port
    suppress its extra taint at the snapshot boundary, or does the artifact
    accept the divergence? — that belongs to whoever owns ADR-0043, not to the
    instrument. Making the standing gate red on a known, escalated disagreement
    teaches the next reader to ignore a red gate, which is how this arc got a
    vacuous one in the first place. So the failure is LOUD, TOTALLED and
    SEPARATE, and `--snapshot-gate` is the switch that becomes the default the
    moment that design question is answered.

    What is deliberately NOT here: a known-failure allow-list. Both of this
    repo's exception tables are empty and a new entry is a finding; an
    expected-failure set for the one class this comparison exists to see would
    re-create the blindness somewhere else.
    """
    fatal = totals["OVER"] + totals["DECLARED-MISMATCH"] + totals["HEADER-MISMATCH"]
    print(f"\nSNAPSHOT TOTAL  MATCH={totals['MATCH']}  UNDER={totals['UNDER']}  "
          f"OVER={totals['OVER']}  DECLARED-MISMATCH={totals['DECLARED-MISMATCH']}  "
          f"HEADER-MISMATCH={totals['HEADER-MISMATCH']}  "
          f"unresolved-only={totals['UNRESOLVED-ONLY']}")
    if invalid:
        print(f"SNAPSHOT INVALID comparisons: {len(invalid)} — "
              f"{', '.join(t['label'] for t in invalid)}")
    if not fatal and not invalid:
        print("SNAPSHOT RESULT: PASS — every row the port would write, "
              "the oracle's record carries.")
        return 0
    print("SNAPSHOT RESULT: FAIL — the port would write a row the oracle's record does not "
          "carry."
          + ("" if gate else
             "\n        NOT GATING (pass --snapshot-gate to make this the exit code); "
             "see issue #116 and `_snapshot_result`."))
    return 1 if gate else 0


def _incomplete(absent):
    if not absent:
        return ""
    noun = "corpus" if len(absent) == 1 else "corpora"
    return (f"\n        INCOMPLETE — {len(absent)} standing {noun} SKIPPED: "
            f"{', '.join(t['label'] for t in absent)}")


if __name__ == "__main__":
    sys.exit(main())
