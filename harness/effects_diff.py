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
"""
import argparse
import contextlib
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

# Header keys excluded from any snapshot comparison (ADR-0043 § 3): the version
# string necessarily differs between the two implementations, and the digest
# covers config both sides read identically.
SNAPSHOT_HEADER_KEYS = {"schema", "rigor", "vocabulary", "config_digest"}

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
    with project_dir(target) as project:
        ref = run_ref(project)
    print(f"\n=== SELF-TEST {label} ===")
    if ref is None:
        print("  INVALID: the reference produced no parseable effects JSON.")
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


def measure(rs_bin, target, show):
    label = target["label"]
    t = time.time()
    with project_dir(target) as project:
        ref = run_ref(project)
        rs = run_rs(rs_bin, project)
    print(f"\n=== {label} ({time.time() - t:.1f}s) ===")
    if ref is None:
        print("  INVALID: the reference produced no parseable effects JSON "
              "— comparison invalid, not delta-free.")
        return None
    if rs is None:
        print(f"  INVALID: rigor-rs failed [{LAST_RS_FAILURE}] "
              "— comparison invalid, not delta-free.")
        return None
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
    return verdicts


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
        broken = 0
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

    totals = Counter()
    invalid = []
    for t in targets:
        v = measure(rs_bin, t, args.show)
        if v is None:
            invalid.append(t)
            continue
        totals.update(v)
    report_skips(absent)

    print(f"\nTOTAL  MATCH={totals['MATCH']}  UNDER={totals['UNDER']}  "
          f"OVER={totals['OVER']}  DECLARED-MISMATCH={totals['DECLARED-MISMATCH']}")
    if invalid:
        print(f"INVALID comparisons: {len(invalid)} — "
              f"{', '.join(t['label'] for t in invalid)}")
    fatal = totals["OVER"] + totals["DECLARED-MISMATCH"]
    if fatal or invalid:
        print("\nRESULT: FAIL — the port may never claim an effect the oracle does not prove.")
        return 1
    # A skipped corpus does not fail the run — the standing set is machine-local
    # and a member may legitimately be absent — but it may not let the run read
    # as a clean pass either. `RESULT: PASS` is the line that gets grepped and
    # quoted into a note, so the incompleteness rides on THAT line and not only
    # on a SKIPPED banner further up.
    print("\nRESULT: PASS — no over-claims. (UNDER is the arc's odometer, not a failure.)"
          + _incomplete(absent))
    return 0


def _incomplete(absent):
    if not absent:
        return ""
    noun = "corpus" if len(absent) == 1 else "corpora"
    return (f"\n        INCOMPLETE — {len(absent)} standing {noun} SKIPPED: "
            f"{', '.join(t['label'] for t in absent)}")


if __name__ == "__main__":
    sys.exit(main())
