# Compat plan Phase 0 — M1/M2 findings (2026-07-18)

Measurement gates from [the compat plan](20260718-compat-next-stage-plan.md).
Both verdicts are decisive.

## M1 — reference self-diff `47ec8625 vs 7a69f142`: Phase 2 is EMPTY

Ran the reference at BOTH pins (`--no-cache`; the cwd cache is NOT
version-invalidated across the bump — both print 0.2.9 / schema 5 — and a
shared `/tmp/.rigor/cache` makes the diff spuriously zero; the tool's
sanity check reproduces exactly the 2 known fixture additions):

| corpus | diags | added | dropped |
|---|---|---|---|
| gitlab-foss lib (4676 files) | 1374 | 0 | 0 |
| gitlab-foss app/models (1224) | 665 | 0 | 0 |
| mastodon models + lib | 153 | 0 | 0 |
| conference-app | 1998 | 0 | 0 |
| concurrent-ruby | 5804 | 0 | 0 |
| mail | 7196 | 0 | 0 |
| textbringer | 63 | 0 | 0 |

(rubocop-ast / liquid / faraday: poison-file batch abort, same fp_audit
limitation; not needed for the verdict.)

**Verdict: the RC's inference-precision deltas (`(?)` return, regex
narrowing, join/Data/Struct folds, non-empty invalidation, void→top) produce
ZERO new reference diagnostics on ~17k measured real-corpus diagnostics.
NONE gets ported on corpus grounds** — the five-slices-0-gaps lesson applied
ex ante. void→top revives only as a Phase-3 prerequisite if
`static.value-use.void` is built.

## M2 — gitlab lib UM residual (179): characterized, 5 GO mechanisms

Cluster histogram by the reference's receiver rendering (tool:
`m2_um_gaps.py`; the earlier "1141" reading was a tool bug — rigor-rs emits a
bare JSON array, the reference wraps in `{"diagnostics":}`):
String 51 / Tuple-Hash-shape 25 / Array[Dynamic] 16 / Hash(+typed) 17 /
Array[String] 9 / Array 6 / nil-bool 6 / Integer 6 / singleton 5 / Class 5 /
Date 5 / Set 5 / rest ~23.

Two structural facts:

1. **~95% of the fired METHOD NAMES are ActiveSupport core-ext**
   (`present?`/`singularize`/`index_with`/`hours`/`exclude?` …). In clean
   mode these methods genuinely don't exist, and the reference fires; with
   the AS plugin configured BOTH engines go silent. So closing these is
   parity-correct AND product-safe (unlike Tier B/C, no FP-safety mechanism
   is deleted).
2. **rigor-rs has NO AS-leniency list** — probed: `[1,2].exclude?`,
   `"abc".singularize`, `{}.deep_merge!` all fire identically to the
   reference. The silence is purely missing RECEIVER-TYPING substrate.

### GO — five mechanisms, each PROVEN by minimal repro (ref fires / rs silent)

| # | mechanism | repro | est. sites |
|---|---|---|---|
| 1 | `CONST = <literal>.freeze` unwrap in the C5 harvest | `A = %w[a b].freeze; A.exclude?("c")` | biggest slice of the 25 shape sites |
| 2 | `Kernel#Array` nominal fallback (S1 sibling) | `Array(c).presence` | part of Array[Dynamic] 16 |
| 3 | `rand` returns (`(int)->Integer`, `()->Float`) | `rand(5).hours` | ~3 |
| 4 | singleton-method RBS returns (`Date.today -> Date`) | `Date.today.end_of_month` | Date 5 + Time 2 + tail |
| 5 | namespaced core singletons (`ERB::Util`) | `ERB::Util.html_escape_once(s)` | 5 |

Per the standing rule each slice still runs its own valid-mode
`fp_audit --gaps` prediction before building.

### DEFERRED — substrate-class (the String-51 bulk)

Implicit-self RBS returns (`name.demodulize`), interprocedural non-literal
method returns, `&:sym` block-pass chains (`.sort_by(&:x).index_by(&:y)`),
flow narrowing (`is_a?(String) &&` / `||= {}`). These are the ADR-0022-class
items; not sliceable now.

## Tooling caveats (for reuse)

- Reference runs MUST pass `--no-cache` for cross-pin comparisons.
- Pass ABSOLUTE file paths (subprocess cwd is `/tmp`; relative paths become
  per-file load-errors that mimic real diagnostics).
- rigor-rs `--format json` emits a bare array; the reference wraps in
  `{"diagnostics": [...]}` with a possible non-JSON preamble.
