# LSP `exclude:` parity — `check`'s stage-1 file filter, applied to the open buffer (2026-07-25)

The [stage-3 parity slice](20260725-lsp-stage3-parity.md) closed `check`'s stage-3
tail and listed what was still divergent. This slice closes its **divergence #2**:
config `exclude:` was honoured by `check` (and by the LSP's overlay DISCOVERY) but
not by the LSP for the **buffer being analysed**, so opening an excluded file
published editor markers `rigor check` would never produce — the same PRESENCE
mismatch class the stage-3 slice closed for `severity: off`.

Blast radius: `crates/rigor-cli/src/lsp.rs` (+ its unit tests),
`crates/rigor-cli/tests/lsp_check_parity.rs`, and one extraction in
`crates/rigor-cli/src/config.rs`. No `rigor-infer` / `rigor-parse` / `rigor-rules`
/ `rigor-index` edit, no change to `check`'s behaviour, no new analysis semantics.
Harness live + snapshot **216/218, 0 unregistered FP** — byte-identical to
pre-slice.

**The first cut of this gate was wrong and was rejected in review.** It re-derived
ONE canonical spelling per buffer and, in doing so, silently dropped three path
forms `check` analyses (B1-B3 below) — regressions against master, not merely gaps.
The design that replaced it, and the invariant it turns on, are the substance of
this note.

## The rule

`check` applies `exclude:` in **stage 1**, as the very first thing it does per
file — before the file is even read:

```rust
// main.rs, analyze_files stage 1 (file-parallel)
.map(|(order, path)| {
    // Config `exclude:` — skip the file entirely before reading it.
    if cfg.is_excluded(path) { return Stage1::Excluded; }
    let source = match std::fs::read_to_string(path) { … };
    if rigor_parse::looks_like_erb_template(source.as_bytes()) { return Stage1::Excluded; }
    …
})
```

and `Stage1::Excluded` is dropped in the serial drain, so the file contributes **no
rows and no AST**.

`Config::is_excluded` globs the path *string as `check` spells it*, not a canonical
absolute path. The spellings come from `expand_check_paths` → `collect_rb_files`,
which builds `<root>/<rel>` by `Path::join`: with the production root the matched
string is `lib/sub.rb`, under `paths: ["."]` it is `./lib/sub.rb`.

### THE INVARIANT

> **A buffer is excluded iff EVERY discovery spelling of that file is excluded.**

One file reaches discovery under **several** names, and `check` analyses it if
**any** of them survives `exclude:`:

- a symlinked `.rb` is walked under the **link's** name — `collect_rb_files`
  includes symlinked files deliberately (`main.rs`, the 2026-07-06 audit correction
  matching `Dir.glob`) — while its content lives at a different canonical path;
- overlapping `paths:` roots (`[".", "lib"]`) walk the same file **twice**, under
  two spellings that can carry opposite verdicts.

A gate that re-derives one canonical spelling cannot express this, which is exactly
how the first cut produced B1-B3.

## The gate, in three tiers

Reached from `compute_diagnostics`, first thing, in `check`'s stage-1 order.

**Tier 1 — discovery MEMBERSHIP (the primary signal).** `ProjectContext::overlay`
already holds the canonical path of every file in the POST-`exclude:` discovery
set — i.e. exactly the files `check` analyses. Buffer present ⇒ some spelling
survived ⇒ **not excluded**. No spelling arithmetic at all.

**Tier 2 — spelling fallback, when the overlay cannot answer** (guard tripped,
empty project, a new/unsaved buffer, a buffer outside `paths:`). Every candidate
spelling of the buffer is enumerated and it is excluded only if they are **all**
excluded. Two things make the enumeration right where the first cut was wrong:

- the buffer contributes **three names**, not one — `decoded` (literally what the
  editor sent, the spelling `rigor check <file>` receives and the only one that
  survives a symlinked DIRECTORY), `named` (decoded with its *directory* resolved
  but the file NAME kept — the name discovery walks a symlinked `.rb` under), and
  `canonical` (fully resolved, the overlay key);
- each name is re-spelled under **every** configured root that contains it, not the
  first — otherwise the answer depends on `paths:` order (B3). A name under no
  configured root falls back to the project-root-relative spelling.

**Tier 3 — confirm before dropping.** The buffer's own names may all be excluded
while the file still reaches discovery under a name the buffer does not carry
(`lib/link.rb` → `lib/real.rb` with `exclude: ["lib/real.rb"]`). Dropping output is
the consequential direction, so it is confirmed against the REAL walk
(`discovery_spellings`, the function `project_files` itself now calls) before it
happens.

Only **symlinked** spellings are consulted there, and that is complete rather than a
shortcut: a non-symlink spelling names its own file, and every such spelling of the
buffer is already among tier 2's candidates. The only name tier 2 structurally
cannot see is a symlink elsewhere in the tree pointing at the buffer.

> **Cost.** Round-2 review re-measured the same shape on a RELEASE build at
> **~40 ms** for tiers 2+3 (walk 11 ms, glob 1.5 ms, ~27 ms of `symlink_metadata`),
> not the ~6 ms first recorded here — and because an excluded buffer is by
> definition absent from the overlay, tier 1 never answers for it, so this is paid
> on EVERY post-debounce dispatch in that buffer and scales with project size. It is
> off the loop thread and the publish is empty either way, so it is not urgent;
> memoizing the tier-3 answer per (canonical path, `generation`) would remove it.
> The original comparison stands directionally: **~25-40 ms** for the naive form that
> `canonicalize`s every surviving spelling (`realpath` walks the whole path per
> file). An `lstat` per candidate answers the common case; the full resolve is paid
> only for real symlinks. Tier 3 runs ONLY for a buffer already judged excluded — a
> dispatch that publishes nothing either way — so it never sits on the latency path
> of a buffer that is getting diagnostics.

The glob itself is **not re-implemented**: `Config::is_excluded`'s body was
extracted to `config::matches_exclude(patterns, path)` and `is_excluded` delegates,
so `check` and the LSP match with one authority (invalid-pattern inertness
included). Likewise `project_files` now delegates its walk to `discovery_spellings`,
so tier 3 consults the same walk discovery does and cannot drift from it.

The gate returns an **empty diagnostic set**, not "no publish": the caller publishes
it, which is what CLEARS any markers the editor already holds for that URI — the
same empty-publish `didClose` uses.

### ERB consistency (asked for explicitly, and it holds)

Both tools call the same predicate, `rigor_parse::looks_like_erb_template`, in the
same position — `check` on the file's bytes right after reading it, the LSP on the
buffer's bytes. The only difference is the input, and the buffer is the correct one
(a dirty buffer that has become — or stopped being — a template is classified on
what the user is actually editing). `exclude:` runs BEFORE the ERB skip on both
sides.

### Where the gate lives

`ExcludeMatcher` sits on **`ProjectContext`**, beside `disable: SuppressSet` and the
stage-3 `stamp` — the `SeverityStamp` precedent, for the same reason: every field is
config-derived, so `invalidate` / `swap_project` rebuild it for free and the S4
generation guard covers it with no new concurrency reasoning. Nothing re-reads
`.rigor.yml` per dispatch, and the common (no `exclude:`) case short-circuits on an
empty pattern list before any filesystem call. `swap_project` gained a
`ctx: &ServerContext` parameter — the session-stable project root is the one input
the matcher needs that `Session` did not already carry.

## The three regressions (B1-B3) — measured, before and after

All three are E2E tests in `lsp_check_parity.rs`: two real processes, one on-disk
project, `check` asked first and the LSP compared against its answer.

| | Shape | `check` | LSP before | LSP after |
|---|---|---|---|---|
| **B1** | `lib/shared.rb` → symlink to `vendor/shared.rb`; `exclude: ["**/vendor/**"]`, `paths: ["lib"]` | 1 finding (discovery walks `lib/shared.rb`, which the pattern does not cover) | `[]` | 1 finding |
| **B2** | `lib/link.rb` → `lib/real.rb`; `exclude: ["lib/real.rb"]` | 1 finding under `lib/link.rb` (and nothing under `lib/real.rb`) | `[]` | 1 finding |
| **B3** | `paths: [".", "lib"]`, `exclude: ["./lib/**"]` | 1 finding (`./lib/a.rb` pruned, `lib/a.rb` kept) | `[]` | 1 finding |

B3 is driven in **both** `paths:` orders, because the first cut returned on the
first containing root and so gave an order-dependent answer.

> **Non-vacuity**: restoring the pre-review gate (one canonical spelling, first root
> wins) fails all three with `left: [] right: [(2, 3, "error",
> "call.undefined-method", …)]` — and leaves the six pre-existing parity tests
> green, which is why the original matrix did not catch it.

## The mirror case, and N2 — decided and stated

**The mirror case** (`exclude: ["lib/link.rb"]` with the same symlink: `check`
reports nothing under the excluded link name but reports the diagnostics under
`lib/real.rb`): **not a divergence — a naming-level artifact, and the gate
deliberately analyses the buffer.** The file's content IS analysed by `check`, under
the surviving name; suppressing the editor's markers would hide a finding CI
reports. The LSP has no path label to disagree about — it publishes to the URI the
user opened — so "analyse it" is both the invariant's answer and the safe one. This
is precisely the case tier 3 exists for, and it is in the differential matrix
(`exclude: ["lib/real.rb"]`, buffer opened as `lib/real.rb`).

**N2** (a symlinked *directory* `lib/vendor` excluded by `lib/vendor/**`):
discovery does NOT traverse symlinked directories, so the file never reaches the
overlay and tier 1 cannot answer; tier 2's **decoded** spelling is
`lib/vendor/x.rb`, which the pattern matches, and the canonical name is outside
every root. **Measured in round-2 review: the buffer is ANALYSED, not excluded** —
the project-root-relative fallback contributes `vendored/x.rb`, the pattern does not
match it, so `all` fails and the gate returns false. (With BOTH spellings excluded it
does return true.) This is the safe direction — the LSP shows a marker rather than
hiding one — but N2 is NOT closed; it stays on the divergence list.

## Acceptance results

All pass. Each is **non-vacuous, proved by mechanically re-breaking the fix** (one
break at a time, reverted after).

### 1. Excluded-buffer presence parity — PASS

`lsp_honours_config_exclude_exactly_as_check_does` (E2E). Fixture: the S4b
cross-file `Base#helper` / `Sub` override on `lib/sub.rb` plus `lib/typo.rb`
(single-file `call.undefined-method`). CONTROL first with no `.rigor.yml`: both fire
and the LSP equals `check` on both. Then `exclude: ["lib/sub.rb"]` ⇒ both tools
report nothing for it.

> **Non-vacuity**: `if false && project.exclude.excludes(…)` ⇒ FAILS —
> `left: [(4, 7, "warning", "def.override-visibility-reduced", …)] right: []`.

### 2. Non-excluded sibling still works — PASS

Same test: `lib/typo.rb` still yields its one finding in both tools.

> **Non-vacuity**: an over-broad gate (exclude every buffer once any pattern
> exists) ⇒ FAILS at "excluding one file must not silence another" —
> `left: [] right: [(2, 3, "error", "call.undefined-method", …)]`.

### 3. Clearing — PASS in the reachable part; the config-change transition is NOT reachable, and here is why

The spec's literal scenario — a buffer that BECOMES excluded via a config change →
invalidate — **cannot be exercised deterministically, and not for a test-harness
reason**: `.rigor.yml` is read exactly ONCE, at startup, and `invalidate` rebuilds
from the same `st.cfg` (matching the reference's `ProjectContext#invalidate!`). That
is pre-existing divergence #5 below. Editing `.rigor.yml` mid-session cannot move
ANY config-derived behaviour — `disable:`, the severity stamp, and now `exclude:`
alike — so a test asserting the transition would assert a mechanism the server does
not have. It is covered in two halves instead:

- **Publish side** (`integration_excluded_buffer_publishes_empty_and_a_sibling_still_fires`,
  in-process over the real loop): an excluded buffer's `didOpen` produces an actual
  `publishDiagnostics` carrying an **empty array** — a publish, not a silent skip,
  which is what clears markers. A `didChangeConfiguration` then re-analyses every
  open buffer and the excluded one comes back **empty again**, while the sibling
  still reports 1 — proving `swap_project` carried the gate across the rebuild.
- **Config side** (`swap_project_rebuilds_the_exclude_matcher_from_the_session_config`):
  `st.cfg` gains an `exclude:` entry covering the open buffer and `swap_project` is
  driven directly; the rebuilt context's gate now excludes it. This is the seam a
  config-reload slice would feed.

> **Non-vacuity**: (publish side) the `if false &&` break ⇒ FAILS with the
> `call.undefined-method` diagnostic present. (config side) building from
> `&Config::default()` instead of `&st.cfg` ⇒ FAILS at "no stale matcher survives".

### 4. Discovery ↔ per-buffer agreement — PASS (the widened differential)

`exclude_gate_agrees_with_bare_check_discovery`. The matrix now covers **4 `paths:`
shapes** (`["lib"]`, `["."]`, and BOTH orders of the overlapping `[".", "lib"]`) ×
**9 pattern sets** × **2 root spellings** (absolute, and the production
`.`-through-a-symlink shape) × **2 gate tiers** (overlay Live ⇒ membership answers;
overlay Off ⇒ tiers 2+3 must reach the same verdict alone) = **144 runs**, up from
24. Every fixture now contains **two symlinked `.rb` files** — one pointing out of
`lib`, one pointing inside it. Those are the two axes review N1 identified as
missing, and either one breaks the pre-review code.

Ground truth is the invariant itself: a file is analysed iff SOME discovery spelling
of it survived, keyed on canonical identity. Each buffer is constructed from the URI
an editor would send for that discovery spelling — through the link, through the
alias — which is what makes the symlink axis observable at all.

> **Non-vacuity**: the pre-review gate ⇒ FAILS at
> `[paths: ["lib"] / exclude: ["**/vendor/**"] / Absolute / Live] … disagree about
> …/lib/shared.rb`. Removing tier 3 ⇒ FAILS at
> `[paths: ["lib"] / exclude: ["lib/real.rb"] / DotThroughSymlink / Off] … disagree
> about lib/real.rb`. Removing tier 3's symlink resolve ⇒ the same failure.

Plus focused regression tests at the matcher seam:
`exclude_gate_never_drops_a_symlinked_file_check_analyses` (B1 + B2, each with a
control proving the gate still excludes when NO spelling survives) and
`exclude_gate_needs_every_root_spelling_excluded` (B3, both root orders, with a
both-spellings-excluded control).

**Two mechanisms are deliberately NOT discriminated by the matrix, stated so a
future reader does not mistake them for safety mechanisms** (the same honesty the
stage-3 note applied to the bleeding-edge gate):

- **Tier 1 is a fast path, not the correctness mechanism.** Disabling it leaves the
  whole matrix green, because tiers 2+3 are exact on their own. It is kept because
  it answers the common case (a buffer in the project) with a pointer comparison
  instead of a walk. In-overlay ⇒ survives discovery, so it can never disagree.
- **Tier 2's `all` IS load-bearing — do not weaken it to `any`.** (Round-2 review
  corrected an earlier claim here that it was undiscriminated.) Flipping it fails
  the 144-run matrix AND the dedicated B3 test, because B3's surviving spelling
  (`lib/a.rb` under overlapping roots) is not a symlink, so tier 3 structurally
  cannot rescue it. `all` and the symlink-only tier 3 are COMPLEMENTARY: `all`
  covers surviving non-symlink spellings, tier 3 covers symlink aliases.

**N3** (review): under `RootSpelling::Absolute` the relative patterns match nothing
on either side, so those cells agree vacuously. They are kept — they cost nothing
and guard the absolute-root path — and the observation is recorded in the test. The
discriminating cells are the `DotThroughSymlink` ones, which are the production root
shape anyway.

### 5. A buffer OUTSIDE `paths:` but not excluded — decided, behaviour UNCHANGED

Divergence #6 (S4b's N5) says an out-of-`paths:` buffer is analysed against the FULL
project index while `rigor check <that file>` builds an index from that file alone.
**This slice does not change that.** The concerns are orthogonal: `exclude:` decides
*whether* the buffer is analysed; N5 is about *which index*. Not excluded ⇒ nothing
changes. Excluded ⇒ the buffer publishes nothing, which is what the only `check` run
that would report on it also reports. The gate only removes output, and only where a
`check` run agrees, so it narrows the gap and cannot widen it.

### 6. S1–S4b + stage-3 preserved — PASS

All 66 pre-slice `lsp::` tests unchanged and green (debounce, 3-axis stale-drop,
overlay + conservative re-harvest, the incremental-vs-full-rebuild differential,
guard hysteresis, the severity stamp, single-writer, no-lost-update,
panic-never-stuck, shutdown-no-hang), plus 8 new ⇒ **74**. The 6 pre-existing E2E
parity tests still pass unchanged, plus 3 new (B1/B2/B3) ⇒ **9**.

## Still divergent between the LSP and `check` after this slice

The stage-3 list, updated. Its item 2 (`exclude:`) is CLOSED and removed.

1. **No baseline (ADR-22).** `check` applies `apply_baseline` after the stamp; the
   LSP has no baseline at all, so a diagnostic a project has baselined is still
   published to the editor.
2. **A panicking buffer produces NOTHING, not `internal-error`.** `check` pushes a
   synthetic `internal_error_diag` finding (and an stderr line);
   `compute_diagnostics`'s `catch_unwind` returns an empty list. The stage-3 stamp's
   `internal-error` bypass is insurance for the day that path is added; it is not
   exercised end to end today.
3. **The project root is the process cwd** (S4b's deferred N4): `rootUri` /
   `workspaceFolders` are not consulted, so a server launched from the wrong
   directory discovers zero project files and — because an empty project
   deliberately does not trip the scale guard — discloses nothing. This slice
   inherits that root, so `exclude:` is interpreted against the cwd exactly as
   `paths:` is.
4. **`rigor lsp` accepts no `--bleeding-edge` flag.** Only `bleeding_edge:` in
   `.rigor.yml` reaches the editor, so `check --bleeding-edge=…` and the LSP can
   disagree on the rule set for the same project.
5. **`.rigor.yml` is read ONCE, at startup.** `invalidate` rebuilds `disable`, the
   stamp AND (now) the exclude matcher from `st.cfg`, but never re-parses the config
   file, so editing `severity_profile:` / `disable:` / `exclude:` mid-session takes
   effect only after a restart. All three are written to follow `st.cfg` so they come
   along for free when a slice makes the config reloadable. This is what makes
   acceptance 3's transition unreachable, and it is now the highest-value remaining
   LSP slice.
6. **Buffers outside `paths:`** are analysed against the FULL project index (S4b's
   N5). Narrowed, not closed, by this slice: such a buffer is now correctly SILENT
   when `exclude:` covers it, but when it is not excluded the index divergence
   stands.
7. **hover / completion / documentSymbol still use the single-file index** (S4b, by
   design: they answer synchronously under a <100 ms p95 budget). Related and new:
   they are **not** `exclude:`-gated either — hovering inside an excluded file still
   answers. Deliberate for this slice (`check` has no hover, so there is no parity
   claim to make about it), recorded rather than silently done.
8. **A file discovery keeps but the overlay could not HARVEST** (a broken symlink, an
   unreadable file, an ERB template, or one that panics the parser on disk) is not in
   tier 1's set. Tiers 2+3 then decide it, which is the exact path — so this is
   listed for completeness, not as a known wrong answer: the tiers agree by
   construction, and the differential drives both.

The parity claim after this slice: **for a saved or dirty buffer inside `paths:`, in
a project with no baseline, the LSP's published diagnostics equal
`rigor check <project>`'s rows for that file on (rule id, line, column, severity,
message) — and the buffer is silent exactly when no discovery spelling of its file
survives `exclude:`.**

## Gates

- `cargo build --offline && cargo test --offline`: PASS — workspace green
  (349 + 228 + 157 + 63 + 48 + 41 + 9 + 4 + 3, 0 failed).
- **5× flake check**: the `lsp::` test binary **74/74 five times, 0 failures**;
  `lsp_check_parity` **9/9 five times**; both **once more under
  `RAYON_NUM_THREADS=1`** (74/74 and 9/9).
- `ruby harness/run.rb` + `ruby harness/run_snapshot.rb`: **PASS, 216/218,
  0 unregistered FP** (unchanged from pre-slice — the LSP is not on the harness's
  path, which is the point of the blast-radius bound). Run against the POPULATED
  submodule pin `7a69f142`, per `UPSTREAM.md` oracle hazard 3 — pointing
  `REFERENCE_RIGOR_DIR` at a working checkout 56 commits ahead of the pin reported
  213 phantom FPs on this same tree.
- `python3 harness/docs_check.py`: PASS (4 budgets, links resolve).
- `cargo clippy --workspace --all-targets -- -D warnings` in a fresh
  `CARGO_TARGET_DIR`: clean.
