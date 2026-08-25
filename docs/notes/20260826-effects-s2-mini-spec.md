# Effects slice 2 mini-spec — direct summaries, no typer (2026-08-26)

Implements ADR-0043 slice 2 from [the probe](20260826-effects-s2-probe.md),
whose §3 mechanics and §5 predicted verdict table are NORMATIVE for this
slice — the spec below sets the frame; the probe note carries the per-handler
and per-construct detail the implementation must match.

## The four grading traps (each verified in `effects_diff.compare()`)

1. **Exhaustiveness**: emit `"exhaustive": false` on every method — grades
   UNDER (`:265` fires only on `sex and not rex`), never OVER. `causes` is
   ungraded; write an honest out-of-enum marker (`port-incomplete`).
2. **Declared**: lane-absent == lane-empty (`:238`), compared exactly —
   slice 2 does NOT implement the lane (it is slice 6, the caller-lane
   join). Survival: (a) the gate names 01/02/03 corpora; (b) self-defense —
   a project where a lexical scan finds effect annotations /
   `effects.attribution:` config emits `"methods": {}` (an empty map always
   under-claims). **Must-still-fire control**: an annotation-free project
   MUST emit real methods (a test pins both directions — suppressions
   without controls are how this repo got burned three times).
3. **Proven = raw string subset (`:259`), NOT label subsumption**: emitting
   `io.fs` where the oracle proved `io.fs.read` is OVER. The safe fallback
   under uncertainty is **∅, never the parent label** — the inverse of
   upstream's handler contract. Consequence: the six non-plugin narrowing
   handlers are MANDATORY in this slice, and slice 1's `lookup_with` must
   not be consumed raw for narrowed rows (it returns the un-narrowed entry
   — `catalog.rs:430-431`).
4. **The posture tier converts typing precision into proven labels**: this
   slice sidesteps it entirely — see "No typer".

## No typer — the load-bearing scoping

All 11 corpus construct origins are SYNTAX-determined (probe §3): the
collector handles **constant-path receivers and implicit self only**,
declining upstream's `record.receiver_class` third arm. The handled-target
set is thereby a strict subset of upstream's ⇒ only UNDER, and ADR §1
("collection is observational") becomes a dependency fact: the collector
calls no Typer, reads no inference state. Postures stay ON for
constant-path receivers (probe-verified safe: `File.some_uncatalogued` →
`io.fs`).

## Shape

- **Collector + command in `crates/rigor-cli/src/effects/`** (the sig_gen
  precedent: an observational consumer beside the engine, outside the
  `check` path). `crates/rigor-effects` stays data/vocabulary.
- The collector walks the **Prism tree** (`rigor_parse::parse` +
  `pub use ruby_prism`; zero new deps) — the lowered AST cannot carry the
  construct distinctions (cvar vs gvar writes are one anonymous node;
  backtick/`alias`/`undef` are `Node::Other`), and widening it would move
  the all-rules walk. No span→NodeId bridge is needed this slice.
- **Unit identity** per probe §3: `<toplevel>#m`, `C#m`, `C.m` (both
  `def self.` and `class << self`), nested `def` = own unit (encloser ∅),
  in-method `define_method` = `mutate.static` on the encloser AND its own
  unit, `attr_accessor` synthesizes reader+writer, class-body `alias` is
  silent.
- **Mutator sets**: extend `harness/vendor_effects.py` to EXTRACT the three
  set literals from the reference source into a vendored data file in
  `crates/rigor-effects/vendor/effects/` (same drift discipline: `--check`,
  digest test, PROVENANCE update — the §carve-outs paragraph shrinks by
  one). Counts pinned: ARRAY 31 / HASH 15 / STRING 26. Direct-summary
  semantics per probe §3: `[]=`/attr-writer are type-free; other mutator
  selectors without provable ownership SUPPRESS the label (never emit bare
  `mutate`); this deliberately replaces the slice-1 pinned under-claim
  (posture-path `mutates_receiver == false`) — flip that pin.
- **Narrowing handlers**, all six, exact semantics from probe §3:
  `file_open` (absent mode = `"r"`; `"r+"` both; suffix-stripping regex for
  `"wb"`/`"r:UTF-8"`; computed/`File::RDWR` → `io.fs`), `pathname_open`
  (shifted arity), `kernel_open` (leading literal `|` → `io.process`, incl.
  the leading literal fragment of an interpolation), `uri_open` (scheme
  split), `time_new`/`random_new` (**zero positional args** rule; counting
  every lowered arg as positional is the UNDER-safe reading). `sql_verb` is
  plugin-only — out.
- **Command**: minimal `rigor effects --full --format=json` producing
  exactly the grader's shape (probe §2):
  `{"methods": {key: {"effects": [...], "declared": [], "exhaustive": false}}}`
  — exit 0, tolerates `--full`, no non-JSON prefix, cwd = project. `update`
  / `check` / `diff` / text rendering stay slice 5.
- The `rigor-cli → rigor-effects` dependency edge re-enters `rigor-effects`
  into the stale-binary guard scan automatically; update the now-stale
  prose at `harness/effects_diff.py:80-83` in the same commit.

## Acceptance gates (BARE)

1. `cargo test --workspace`; clippy fresh `CARGO_TARGET_DIR` `-D warnings`.
2. **Composition probes before the differential** (it covers 6/420 rows):
   for each narrowing handler and for the ∅-not-parent fallback, a scratch
   project probed against the ORACLE (probe §8's projects seed these;
   reference from the pinned submodule, `.rigor/cache` cleared around runs)
   with the port's JSON matching on those methods.
3. `harness/effects_diff.py --self-test` — the port arm now REAL. The
   acceptance IS the probe's §5 predicted table: per-corpus
   `MATCH/UNDER/OVER/DM` = 01: 3/13/0/0 · 02: 4/11/0/0 · 03: 5/6/0/0 ·
   04: 0/4/0/0 (via the `methods: {}` self-defense) — TOTAL 12/34/0/0,
   PASS. A HIGHER MATCH is suspect (over-reach), not a bonus: stop and
   re-derive before celebrating.
4. `rigor check` byte-identical vs a master binary on mastodon/app (the
   collector must be invisible to check); `harness/run_snapshot.rb` PASS;
   release rebuild + `harness/fp_audit.py --gaps --sweep` 0 FP / 9,204,
   gap set unchanged.
5. Fix the two corpus fixture COMMENTS the probe flagged
   (`owns_what_it_mutates` claims `mutate.local`, oracle says
   `unknown-ownership`; `mutates_its_argument` does not measure
   `mutate.instance`) — comments only, fixtures unchanged.

## Non-goals

Taint producers + `causes` enum + the `resolved` bit (slice 3); the
transitive `effects` key (slice 4); snapshot/update/text surfaces and
`--no-tolerated-effects` (slice 5); the declared lane (slice 6); the plugin
effect layer and `sql_verb`; any Typer/receiver-class consumption; any
`rigor check` behavior change.
