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

## The exclusion semantics matched, with citations

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

and `Stage1::Excluded` is dropped in the serial drain (`Stage1::Excluded => {}`),
so the file contributes **no rows and no AST**.

**The path FORM is the whole difficulty.** `Config::is_excluded` globs the path
*string as `check` spells it*, not a canonical absolute path. `check`'s spelling
comes from `expand_check_paths` → `collect_rb_files`, which builds `<root>/<rel>`
by `Path::join` from the roots it was given — for a bare run those roots are
`cfg.paths`, so with the production project root the matched strings are
`lib/sub.rb`, and under `paths: ["."]` they are `./lib/sub.rb`. The LSP's
`project_files` already reproduces exactly that (`join_root(root, p)` +
`collect_rb_files`, then `out.retain(|p| !cfg.is_excluded(p))`), which is why
discovery has always been right.

A buffer, though, arrives at `compute_diagnostics` as a **canonical absolute**
path (`uri_to_canonical_path`). So the gate re-spells it the way discovery would
have:

- **FIRST the discovery spelling.** For each configured root `p`: canonicalize
  `join_root(root, p)`, `strip_prefix` it off the buffer path, and re-join onto the
  root *as spelled* — byte-identical to what `project_files` produced for that same
  file. Roots are canonicalized before the comparison for the same reason
  `touches_configured_root` does it: a symlinked workspace spelling would otherwise
  defeat the match. A `paths:` entry naming a FILE yields an empty remainder and
  the spelling is the entry itself, matching `project_files`' `out.push(joined)`.
- **FAILING THAT, the project-root-relative spelling.** The buffer is inside the
  workspace but outside every `paths:` root, so bare `check` never discovers it and
  the only run that reports on it at all is an explicit `rigor check <that file>`
  from the project root — which globs `exclude:` against exactly that relative
  spelling.
- **`None` outside the workspace, and for a pathless buffer** (untitled /
  non-`file:`): no `check` invocation from this root names the file, so there is no
  spelling to match and the buffer is left alone.

The glob itself is **not re-implemented**: `Config::is_excluded`'s body was
extracted to `config::matches_exclude(patterns, path)` and `is_excluded` now
delegates to it, so `check` and the LSP match with one authority (invalid-pattern
inertness included). A second implementation of the glob rule is precisely the
drift that produced this divergence.

The gate returns an **empty diagnostic set**, not "no publish": the caller
publishes it, which is what CLEARS any markers the editor already holds for that
URI — the same empty-publish `didClose` uses.

### ERB consistency (asked for explicitly, and it holds)

Both tools call the same predicate, `rigor_parse::looks_like_erb_template`, in the
same position — `check` on the file's bytes right after reading it, the LSP on the
buffer's bytes. The only difference is the input, and using the buffer is the
correct one (a dirty buffer that has become — or stopped being — a template is
classified on what the user is actually editing, not on a stale disk copy).
`exclude:` runs BEFORE the ERB skip on both sides; the gate was placed first in
`compute_diagnostics` to keep that order literal, though both branches return the
same empty set so the order is not observable.

## The wiring

`ExcludeMatcher` (new, `lsp.rs`) carries `root` (as spelled), `paths`, and the
`exclude:` `patterns`. It lives on **`ProjectContext`**, beside `disable:
SuppressSet` and the stage-3 `stamp` — the `SeverityStamp` precedent, for the same
reason: every field is config-derived, so `invalidate` / `swap_project` rebuild it
for free and the S4 generation guard covers it with no new concurrency reasoning.
Nothing re-reads `.rigor.yml` per dispatch, and the common (no `exclude:`) case
short-circuits on an empty pattern list before any filesystem call.

`swap_project` gained a `ctx: &ServerContext` parameter — the session-stable
project root is the one input the matcher needs that `Session` did not already
carry. All four call sites already had `ctx` in scope.

## Acceptance results

All pass. Each is **non-vacuous, proved by mechanically re-breaking the fix** and
observing the failure (the breaks below were applied one at a time and reverted).

### 1. Excluded-buffer presence parity — PASS

`lsp_honours_config_exclude_exactly_as_check_does` (E2E, real `rigor lsp` + real
`rigor check`, two processes, one on-disk project). Fixture: the S4b cross-file
`Base#helper` / `Sub` override (⇒ `def.override-visibility-reduced` on
`lib/sub.rb`, a purely cross-file finding) plus `lib/typo.rb` (⇒
`call.undefined-method`, purely single-file). The CONTROL runs first with **no**
`.rigor.yml`: both files fire, and the LSP equals `check` on both. Then
`exclude:\n  - "lib/sub.rb"` is written and BOTH tools report nothing for
`lib/sub.rb` — asserted as an equality, with the control being what makes the
empty-vs-empty comparison mean something.

> **Non-vacuity**: `if false && project.exclude.excludes(path)` ⇒ FAILS —
> `left: [(4, 7, "warning", "def.override-visibility-reduced", …)] right: []`.

### 2. Non-excluded sibling still works — PASS

Same test, second half: under the same `exclude:`, `lib/typo.rb` still yields its
one finding in both tools (`check` asserted non-empty as its own control).

> **Non-vacuity**: replacing the gate with `if path.is_some() &&
> !project.exclude.patterns.is_empty()` (exclude EVERY buffer once any pattern
> exists) ⇒ FAILS at "excluding one file must not silence another" —
> `left: [] right: [(2, 3, "error", "call.undefined-method", …)]`.

### 3. Clearing — PASS in the part that is reachable; the config-change transition is NOT, and here is why

The spec's literal scenario — *a buffer that BECOMES excluded via a config change
→ `didChangeConfiguration` / `didChangeWatchedFiles` → invalidate* — **cannot be
exercised deterministically today, and not because of a test-harness limitation**:
`.rigor.yml` is read exactly ONCE, at startup, and `invalidate` rebuilds from the
same `st.cfg` (matching the reference's `ProjectContext#invalidate!`). That is
pre-existing divergence #6 below, and closing it was out of scope. Editing
`.rigor.yml` mid-session therefore cannot move ANY config-derived behaviour —
`disable:`, the severity stamp, and now `exclude:` alike — so a test asserting the
transition would be asserting a mechanism the server does not yet have. Rather
than ship an untested claim, the transition is exercised in two halves that
together cover it:

- **The publish side** (`integration_excluded_buffer_publishes_empty_and_a_sibling_still_fires`,
  in-process over the real loop): an excluded buffer's `didOpen` produces an
  actual `publishDiagnostics` carrying an **empty array** — a publish, not a silent
  skip, which is exactly what clears an editor's markers. A `didChangeConfiguration`
  then re-analyses every open buffer and the excluded one comes back **empty
  again** (not with regained markers), while the sibling still reports 1 — which is
  what proves `swap_project` carried the gate across the context rebuild.
- **The config side** (`swap_project_rebuilds_the_exclude_matcher_from_the_session_config`):
  starting from a context that excludes nothing, `st.cfg` gains an `exclude:` entry
  covering the open buffer and `swap_project` is driven directly; the rebuilt
  context's gate now excludes it (and the generation bumped). This is the seam a
  config-reload slice would feed, so the day `.rigor.yml` becomes reloadable the
  transition works with no further change.

> **Non-vacuity**: (publish side) the same `if false &&` break ⇒ FAILS —
> "an excluded buffer publishes an EMPTY set (clearing any markers): [Diagnostic {
> … code: Some(String("call.undefined-method")) … }]".
> (config side) building the matcher from `&Config::default()` instead of
> `&st.cfg` ⇒ FAILS at "the rebuilt context's gate follows `st.cfg` — no stale
> matcher survives".

### 4. Discovery ↔ per-buffer agreement — PASS (the spec's "a mismatch there would be its own divergence")

`exclude_gate_agrees_with_bare_check_discovery` is a differential over the FULL
matrix: 2 `paths:` shapes (`["lib"]`, `["."]`) × 6 pattern sets (empty,
`**/vendor/**`, `**/b.rb`, `**/*.rb`, `lib/a.rb`, `./lib/a.rb`) × 2 root spellings
(absolute temp dir, and the production `.`-through-a-symlink shape) = 24 runs. For
every file bare-`check` discovery walks, it asserts the per-buffer gate answers
**exactly** what discovery's own `exclude:` filter answered. It carries its own
loop non-vacuity guards (the catch-all pattern must prune everything; the empty
pattern set must prune nothing), so the comparison is provably exercised on both
answers.

> **Non-vacuity**: this is the test that catches the naive implementation. Dropping
> the discovery-spelling pass (leaving only the root-relative one — i.e. "strip the
> project root and match") ⇒ FAILS —
> `["paths:\n  - \".\"\nexclude:\n  - \"lib/a.rb\"\n" / DotThroughSymlink] the
> buffer gate and bare-\`check\` discovery disagree about ./lib/a.rb  left: true
> right: false`. Under `paths: ["."]` discovery spells the file `./lib/a.rb`, which
> `lib/a.rb` does not match — so the naive gate would have silenced a buffer
> `check` reports on.

Two scope tests pin the edges: `exclude_gate_leaves_pathless_and_out_of_workspace_buffers_alone`
(an untitled buffer and a file in another workspace are never excluded, with an
in-project control proving the matcher is live) and
`exclude_gate_uses_the_root_relative_spelling_outside_paths` (the fallback, with a
non-matching-pattern control).

> **Non-vacuity** (fallback): returning `None` instead of the root-relative
> spelling ⇒ FAILS at "an out-of-`paths:` buffer is excluded exactly when
> `rigor check spec/x_spec.rb` from the project root would report nothing for it".

### 5. A buffer OUTSIDE `paths:` but not excluded — decided and stated, behaviour UNCHANGED

Divergence #7 (S4b's N5) says an out-of-`paths:` buffer is analysed by the LSP
against the FULL project index, while `rigor check <that file>` builds an index
from that file alone. **This slice does not change that, deliberately.** The two
concerns are orthogonal: `exclude:` decides *whether the buffer is analysed at
all*; N5 is about *which index* it is analysed against. The interaction is clean
in both directions:

- Not excluded ⇒ nothing changes: the buffer is analysed exactly as before,
  against the full project index, and #7 stands verbatim.
- Excluded (by the root-relative spelling) ⇒ the buffer publishes nothing, which
  is what the ONLY `check` run that would ever report on it — an explicit
  `rigor check <that file>` from the project root — also reports. So the gate
  *narrows* #7's gap rather than widening it, and it can never widen it: the gate
  only ever removes output, and it only removes it where a `check` run agrees.

### 6. S1–S4b + stage-3 preserved — PASS

All 66 pre-slice `lsp::` tests unchanged and green (debounce, 3-axis stale-drop,
overlay + conservative re-harvest, the incremental-vs-full-rebuild differential,
guard hysteresis, the severity stamp, single-writer, no-lost-update,
panic-never-stuck, shutdown-no-hang), plus the 6 new ones ⇒ **72**. The 5
pre-existing E2E parity tests still pass unchanged ⇒ **6**.

## Still divergent between the LSP and `check` after this slice

The stage-3 list, updated. Item 2 is now CLOSED and removed; everything else is
carried forward verbatim except where this slice narrowed it. Everything below is
**known and unfixed**, not "probably fine".

1. **No baseline (ADR-22).** `check` applies `apply_baseline` after the stamp; the
   LSP has no baseline at all, so a diagnostic a project has baselined is still
   published to the editor. Explicitly out of scope for this slice.
2. **A panicking buffer produces NOTHING, not `internal-error`.** `check` pushes a
   synthetic `internal_error_diag` finding (and an stderr line); `compute_diagnostics`'s
   `catch_unwind` returns an empty list. The stage-3 stamp's `internal-error` bypass
   is correct-by-construction insurance for the day that path is added; it is not
   exercised end to end today.
3. **The project root is the process cwd** (S4b's deferred N4): `rootUri` /
   `workspaceFolders` are still not consulted, so a server launched from the wrong
   directory discovers zero project files and — because an empty project
   deliberately does not trip the scale guard — discloses nothing. This slice
   inherits that root, so an `exclude:` pattern is interpreted against the cwd for
   the same reason `paths:` is.
4. **`rigor lsp` accepts no `--bleeding-edge` flag.** Only `bleeding_edge:` in
   `.rigor.yml` reaches the editor. Deliberate (an editor launches the server, so a
   per-invocation CLI flag has no user), but `check --bleeding-edge=…` and the LSP
   can disagree on the rule set for the same project.
5. **`.rigor.yml` is read ONCE, at startup.** `invalidate` rebuilds `disable`, the
   stamp AND (now) the exclude matcher from `st.cfg`, but never re-parses the
   config file (matching the reference's `ProjectContext#invalidate!`), so editing
   `severity_profile:` / `disable:` / `exclude:` mid-session does not take effect
   until the server restarts. All three are *written* to follow `st.cfg` so they
   come along for free when a future slice makes the config reloadable. This is
   what makes acceptance 3's transition untestable today (see above), and it is now
   the single highest-value remaining LSP slice.
6. **Buffers outside `paths:`** are analysed against the FULL project index (S4b's
   N5) — better informed than `rigor check <that file>`, but not the same answer.
   Narrowed, not closed, by this slice: such a buffer is now correctly SILENT when
   `exclude:` covers it, but when it is not excluded the index divergence stands.
7. **hover / completion / documentSymbol still use the single-file index** (S4b, by
   design: they answer synchronously under a <100 ms p95 budget). Related and new:
   they are **not** `exclude:`-gated either — hovering inside an excluded file still
   answers. That is deliberate for this slice (`check` has no hover, so there is no
   parity claim to make about it) and is recorded rather than silently done.

The parity claim after this slice: **for a saved or dirty buffer inside `paths:`,
in a project with no baseline, the LSP's published diagnostics equal
`rigor check <project>`'s rows for that file on (rule id, line, column, severity,
message) — and where `exclude:` covers the buffer, both are empty.** That is the
stage-3 claim minus its `exclude:` caveat, and it is the whole claim.

## Gates

- `cargo build --offline && cargo test --offline`: PASS — workspace green
  (347 + 228 + 157 + 63 + 48 + 41 + 6 + 4 + 3, 0 failed).
- **5× flake check**: the `lsp::` test binary **72/72 five times, 0 failures**;
  `lsp_check_parity` **6/6 five times**; both **once more under
  `RAYON_NUM_THREADS=1`** (72/72 and 6/6).
- `ruby harness/run.rb` + `ruby harness/run_snapshot.rb`: **PASS, 216/218,
  0 unregistered FP** (unchanged from pre-slice — the LSP is not on the harness's
  path, which is the point of the blast-radius bound).
- `python3 harness/docs_check.py`: PASS (4 budgets, links resolve).
- `cargo clippy --workspace --all-targets -- -D warnings` in a fresh
  `CARGO_TARGET_DIR`: clean.

**Measurement note for the next session**: `harness/run.rb` against a
`REFERENCE_RIGOR_DIR` pointing at an unrelated local checkout of the reference
reported **213 unregistered FPs**; against the PINNED submodule
(`git submodule update --init reference/rigor`, `7a69f142`) the same tree reports
**0**. The oracle is the pin, not "some rigor checkout" — the [[oracle-and-clippy-gotchas]]
lesson in a new costume.
