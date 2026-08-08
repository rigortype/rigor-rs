# Partially-literal constant harvesting — mini-spec (2026-08-08)

Turns the [probe evidence](20260808-partial-constant-harvest-probes.md) into an
ordered slice plan. The probes overturned the framing this track was parked
under: the reference NEVER declines a partially-literal constant (it keeps a
full shape and types a lambda as `Proc`), so C5's all-or-nothing decline is a
rigor-rs artifact, and — the bigger finding — **C5's project-wide harvest is a
live FP class on master**: the reference's constant-value typing is per-FILE
(a cross-file read of even a fully-literal constant is reference-silent;
probes-note "cross-file" row). Priority order below follows FP-first.

## Ordered slices

### A — per-file constant-value consumption (FP hygiene, FIRST)

Restrict the C5 bare-name arm, the 2e qualified twin, and any other
`ConstLit` consumer to constants harvested from the SAME FILE as the use
site (the harvest may stay project-wide; the CONSUMPTION gate is per-file).

- FIRST widen the probe base: the one probed shape was a same-namespace bare
  read. Probe cross-file with a qualified path, via `include`, and a
  same-file-different-class read, both engines, before fixing the gate shape.
- Census expectation: **no legitimate row can reopen** — a gap row is
  reference-fired, the reference is per-file, so every reference-fired
  constant row is same-file. Any row that disappears from the MATCHED set is
  by definition a rigor-rs-only emission (a latent FP) — record the count.
- Sweep stays 0 FP; fixture rows pin same-file fires + a cross-file silence.

### B — keep partially-literal containers as INERT bare nominals

Widen `const_lit_of`'s container arms: a hash/array literal whose ELEMENTS
are non-literal (lambda values, dynamic elements, splats, non-static keys)
harvests as a bare `Nominal[Hash]`/`Nominal[Array]` — **elements NEVER typed**
(probe z2: the reference leaves a const-read element `Dynamic`; element-typed
harvesting would out-precise the oracle at exactly the projection sites it
declines — an FP generator). Fully-literal harvesting is unchanged
(value-pinned rendering must not regress).

- Safety is the probed INERTNESS: `fold_tuple_projection`/
  `fold_hash_shape_projection` match `Tuple`/`HashShape` only, arity/ATM/
  possible-nil/always-truthy unreachable; the only surfaces that light up are
  `call.undefined-method` (the intent) and `call.raise-non-exception`
  (parity-positive — a container is never an Exception). Pin both with tests.
- **Message rendering diverges** on these rows (`for Hash` vs the reference's
  `for { c: Proc }`): check `harness/run.rb`'s match key BEFORE registering
  fixture rows; if the message is load-bearing, register the divergence
  explicitly rather than skipping the row.
- Yield: the routable_token row (real-row probe fires at `:61:14`); census
  says up to 292 constants newly harvest (+31%) — the diff must be walked and
  any bonus closure oracle-spot-checked; ZERO new rows.
- Reassignment: the reference unions duplicate assignments (probe). C5's
  existing single-assignment gate is a strict under-emit — keep it.

### C — literal-rooted chain constants (ASSESS, likely small or deferred)

`SEV = %w[…].map.with_index.to_h.freeze` types bare `Hash` in the reference.
Candidate mechanism: type the RHS with the existing typer folds (empty env,
same file) and keep a resulting bare `Nominal[Hash]`/`Nominal[Array]`.
The codequality row ALSO needs `Hash#keys` off an argument-less nominal to
produce `Array` (probes-note envelope §2 — two mechanisms). Assess after B:
build only if the fold path already produces the nominal and the `keys`
projection is a small, probed extension; otherwise record the decline with
the two-mechanism evidence. 357 chain constants in the corpora bound the
blast radius — any build must walk its census diff row by row.

## Verification (binding, per slice)

Full gates (`docs_check.py` BARE), sweep 0 FP / 9204, census pre/post with
every moved row explained, fixture `harness/corpus/92_partial_constant_harvest.rb`
(verify 92 free) with oracle-verified lines incl. cross-file controls.

## BUILT — slice A (2026-08-08): per-file constant-value consumption

### Widened probe base (before touching the gate)

Fresh `mktemp -d` cwd per row, `--no-cache`, pinned plugin path, reference at
`c39e6675`; both files in ONE `check` invocation.

| # | shape | reference | rigor-rs (master) | rigor-rs (after) |
|---|---|---|---|---|
| A1 | `module M; class C; L = [1,2].freeze` in `a.rb`; bare `L.zzz` inside the SAME `M::C` in `b.rb` | silent | **fires** `b.rb:4:9 for [1, 2]` | silent |
| A2 | same write; `M::C::L/H/S.zzz` at TOPLEVEL of `b.rb` | silent | silent (2e visibility filter already declined) | silent |
| A3 | `module Mixin; LIMIT = …` in `a.rb`; `class Host; include Mixin; LIMIT.zzz` in `b.rb` | silent | silent (lexical filter already declined) | silent |
| A7 | toplevel `TOPL = [1,2].freeze` + `SCAL = 5` in `a.rb`; **`require_relative "a"`** + reads in `b.rb` | silent | **fires** ×2 | silent |
| A4 | control, ONE file: `module M; class C; L = …; end; class D; def go; M::C::L.zzz` | fires `:7:15` | silent (2e declines a sibling namespace) | silent — unchanged |
| A5 | control, ONE file: toplevel `TOPL` read inside `class K` | fires | fires | fires — parity |
| A6 | control, ONE file: `include Mixin` bare read | silent | silent | silent — parity |

**Probe corrections to the note's framing** (three, all narrowing the exposure):

1. The exposure is **only the bare same-namespace spelling** (A1) and the
   toplevel spelling (A7/x2). The qualified twin (A2) and the `include` route
   (A3) were *already* silent on master — the §7c lexical-visibility filter
   covers them, so "the 2e qualified twin" named in the slice text was not in
   fact a live FP surface. The gate is still applied there (2e is a pure
   spelling of the same harvest), it just closes nothing new.
2. **`require_relative` does not change the reference's answer** (A7). The
   per-file rule is about the FILE, not about require reachability — worth
   pinning, because "the reference just doesn't know a.rb is loaded" would have
   predicted the opposite.
3. **Confirmed at the source, not only empirically**:
   `ScopeIndexer#build_in_source_constants(root, …)` walks ONE file's root and
   nothing cross-file feeds `in_source_constants`
   (`reference/rigor/lib/rigor/inference/scope_indexer.rb:1202`,
   `reflection.rb:123`). The only cross-file constant table the reference keeps
   is `discovered_classes` for `Const = Class.new(Super)` — class identity, not
   VALUE. So the correction the spec allowed for ("if probing shows the
   reference is NOT per-file for some spelling") did not arise: it is per-file
   for every spelling probed.

### Implementation

`LoweredAst` gained an intrinsic `file_id` (process-global atomic, one per
`lower()` call, `Clone`-preserved) — the parser never sees a path, and an
extrinsic identity keyed on the AST object is what both the CLI (`build_project`
over `prepared`, then analyze the same `&p.ast`) and the LSP (`overlay_source_index`
REPLACEs the buffer's AST into the same slice it analyzes) already guarantee is
stable. `Debug` is hand-written to OMIT it, so `{:?}` stays a CONTENT rendering:
the LSP's incremental-vs-full differential tests compare two lowerings of
identical bytes that way.

Harvest entries carry the assigning file id (`HarvestedConst`); the HARVEST stays
project-wide (the single-assignment gate must still see every file), only
`literal_constant` / `qualified_literal_constant` gained the `use_file` filter.

The 2b `ENV` negative check was deliberately NOT moved onto the per-file gate: it
now calls a new `literal_constant_visible_any_file`, preserving its exact
file-agnostic decline semantics (unit-pinned by
`env_negative_check_stays_file_agnostic`, which also re-asserts the
`project_writes_constant` decline).

### Gates + census

| gate | result |
|---|---|
| `cargo build --offline && cargo test --offline` | PASS (all suites; 4 new unit tests) |
| `ruby harness/run.rb` (live reference) | PASS — 92 fixtures, 353 matched, 28 gaps, 1 registered extra, **0 unregistered** |
| `ruby harness/run_snapshot.rb` | PASS (identical numbers) |
| `python3 harness/fp_audit.py --gaps --sweep` | **0 FP / 9204 files** |
| `python3 harness/docs_check.py` (bare) | PASS, exit 0 |
| clippy `-D warnings`, fresh `CARGO_TARGET_DIR` | clean (needed one `type HarvestedConst` alias for `type_complexity`) |

Census (`gap_census.py --sweep`), pre/post on the same binary contract:

| | base | after A |
|---|---|---|
| total coverage gaps | 1127 | **1127** |
| mastodon/app · gitlab-foss/lib · mail | 38 · 282 · 539 | unchanged |
| Ruby · dependabot-core · concurrent-ruby · net-ssh · haml/lib | 30 · 74 · 85 · 74 · 287 | unchanged |

Row-by-row diff of the two dumps (keyed `path, line, rule, message`): **0 new
gaps, 0 closed gaps** — the gap SET is identical, not merely the count.

**Slice-A latent-FP count: 0.** Because no row left the MATCHED set, no
accidentally-key-matched rigor-rs emission existed on the sweep. A direct
binary-to-binary diff (master build vs slice-A build, same 9204 files,
parity severities) confirms it from the other side: **0 diagnostics lost, 0
gained, per corpus and in total.**

So slice A closes a probe-demonstrated over-emission class (A1/A7 fired on
master, are silent now, matching the oracle) that the standing sweep set happens
to contain **zero instances of** — exactly what probes-note §7 predicted ("the
sweep is 0 FP today only because the shape is rare"). The value is prospective:
it is the gate that keeps slice B's +31% container harvest from widening a
cross-file exposure proportionally.

### Deviation

The fixture harness checks ONE file at a time (`harness/lib.rb#fixtures` globs
`*.rb`, `run_reference` passes a single path), so the cross-file SILENCE cannot
be a fixture row. `harness/corpus/92_partial_constant_harvest.rb` pins the
POSITIVE half (toplevel-read-in-class, bare read in the defining namespace,
same-file qualified 2e spelling — 3/3 oracle-matched, no gaps, no extras); the
cross-file silences are pinned as Rust unit tests instead
(`constant_value_is_consumed_only_in_the_assigning_file`,
`qualified_constant_value_is_consumed_only_in_the_assigning_file`,
`toplevel_constant_value_is_still_per_file`).

## Non-goals

- No element typing anywhere in B/C. No shape unions for reassignment.
- No change to the ADR-0033 provenance gates or the 2b `ENV` arm's negative
  check (it consults the harvest — verify it still declines what it must).
