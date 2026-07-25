# LSP stage-3 parity tail — the ADR-8 SeverityStamp + the bleeding-edge gate (2026-07-25)

The follow-up [S4b](20260719-lsp-s12-s4b.md) tracked: S4b made the LSP's
`SourceIndex` **the same index `check` builds**, but `compute_diagnostics` still
stopped short of `check`'s stage-3 **tail**. This slice closes that tail. Blast
radius: `crates/rigor-cli/src/lsp.rs` (+ its unit tests) and
`crates/rigor-cli/tests/lsp_check_parity.rs` — no `rigor-infer` / `rigor-parse` /
`rigor-rules` / `rigor-index` edit, no change to `check`'s behaviour, no new
analysis semantics. Harness live + snapshot **216/218, 0 unregistered FP** —
byte-identical to pre-slice.

## What the tail was

Three user-visible defects, all pre-existing since the LSP shipped:

1. **A PRESENCE mismatch, not a severity one.** A rule set to `off` via
   `severity_profile:` / `severity_overrides:` is DROPPED by `check`
   (`main.rs`'s `severity::ResolvedSeverity::Off => continue`) but the editor
   still published a marker for it. A project that had deliberately silenced a
   rule saw it anyway, per keystroke.
2. **Wrong levels.** The editor showed each rule's AUTHORED severity, not the
   profile-resolved one, so a project on `severity_profile: strict` (or any
   non-default profile) saw error/warning/info levels its CI run did not report.
3. **No bleeding-edge plumbing.** `check` gates `rigor_rules::void_value_use_diagnostics`
   on the resolved severity of `static.value-use.void`; the LSP never ran that
   collector, so a project adopting `use-of-void-value` got the rule in CI and
   nothing in the editor.

`grep severity::resolve crates/rigor-cli/src/lsp.rs` returned nothing before this
slice.

## The wiring

- **`SeverityStamp`** (new, `lsp.rs`) carries the ADR-8 resolution inputs:
  `profile` (`cfg.severity_profile()`), `user_overrides`
  (`cfg.severity_overrides()`), `bleeding_overrides`
  (`bleeding_edge::severity_overrides_for(cfg.bleeding_edge_selector())`), and
  `void_rule_active` — the last computed with the SAME `severity::resolve` call
  `check`'s `analyze_files` uses for its memoised rule-activation gate.
- It lives on **`ProjectContext`**, beside the existing `disable: SuppressSet`:
  same config provenance, so `invalidate` / `swap_project` rebuild it for free
  and the S4 generation guard covers it with no new concurrency reasoning.
- **`SeverityStamp::apply(&mut diag) -> bool`** reproduces `main.rs`'s stage-3
  block: map the emitted severity to `ResolvedSeverity`, `severity::resolve`,
  return `false` on `Off` (the caller drops), re-stamp otherwise — and
  **short-circuit `internal-error` before any of that** (the reference's
  `rule.nil?` bypass: a per-file panic must never be silenced by config).
- **`compute_diagnostics`** runs `void_value_use_diagnostics` under
  `stamp.void_rule_active`, in `check`'s position (after
  `shadowed_rescue_diagnostics`, before suppression), and applies the stamp to
  the `rigor_rules::Diagnostic`s **before** `to_lsp_diagnostic` maps them.

**Deliberate deviation from the spec, stated for the record:** the spec asked for
the bleeding-edge SELECTOR to be stored on `ProjectContext` too. It is not — it
would be a dead field. `check` uses the selector for exactly two things, and both
are already reduced into the stamp: the merged override map, and `void_rule_active`
(which `check` derives via `severity::resolve`, not from the selector directly).
Storing it unused would trip `-D warnings`.

## Composition order (verified against `main.rs`, not assumed)

`main.rs`'s stage 3, read top-to-bottom:

```
analyze_with_source_and_folder
  → shadowed_rescue_diagnostics
  → void_value_use_diagnostics        [if void_rule_active]
  → suppression_marker_diagnostics    (into the SAME list, so a marker can
                                       suppress its own complaint)
  → filter_suppressed                 (inline `# rigor:disable`)
  → disable_matcher.suppresses        (config `disable:`)   → continue
  → SeverityStamp                     (Off → continue; else re-stamp)
```

and then, back in `cmd_check` and OUTSIDE `analyze_files`:

```
  → apply_baseline                    (ADR-22, "applied LAST … per reference WD6")
  → prepend_path_errors               (never baseline-suppressed)
```

`compute_diagnostics` now matches that chain exactly, minus the two `cmd_check`
steps that have no LSP counterpart (no baseline in the LSP — out of scope for
this slice; no path arguments in the LSP, so no path errors). In particular
**the stamp runs LAST of the in-`analyze_files` steps** — after both suppression
filters. That ordering matters: the stamp is the only step that can also REWRITE
a diagnostic, so a suppression running after it would be deciding on a re-stamped
severity rather than the authored one.

## Acceptance results

All four pass. Each is **non-vacuous, proved by mechanically re-breaking the
fix** and observing the failure (the breaks below were applied one at a time and
reverted).

### 1. `off` presence parity — PASS

`lsp_drops_an_off_rule_exactly_as_check_does` (E2E, real `rigor lsp` +
real `rigor check`, two processes, same on-disk project). Fixture: the S4b
cross-file `Base#helper` / `Sub` override ⇒ `def.override-visibility-reduced`.
The test runs the CONTROL first (no `.rigor.yml`: `check` and the LSP both report
exactly that one finding), then writes
`severity_overrides:\n  def.override-visibility-reduced: off` and asserts BOTH
are empty. The control is what makes the empty-vs-empty comparison meaningful.

> **Non-vacuity**: replacing the stamp `filter_map` with the pre-slice
> `.map(|(_, d)| to_lsp_diagnostic(…))` ⇒ FAILS —
> `left: [(4, 7, "warning", "def.override-visibility-reduced", …)] right: []`.

### 2. Profile severity parity — PASS

`lsp_publishes_the_profile_resolved_severity_like_check`. Same fixture under
`severity_profile: strict`, which moves `def.override-visibility-reduced` from
its authored `warning` to `error` (asserted in the test as its own control, so
the comparison provably discriminates). The LSP's published diagnostics equal
`check`'s on **(line, column, severity, rule id, message)**.

The comparison tuple gained the **rule id** for this slice: `check_findings` now
parses `rigor check --format json` (the only output shape carrying `rule`)
instead of the text renderer, and the LSP side reads `diagnostic.code`. The two
pre-existing S4b parity tests were migrated onto it unchanged in intent — they
now compare on rule id too.

> **Non-vacuity**: same break as (1) ⇒ FAILS — `left: … "warning" …`,
> `right: … "error" …`.

### 3. `internal-error` bypass — PASS (unit)

`stage3_stamp_never_silences_internal_error`: under a config carrying
`severity_overrides:\n  internal-error: off\n  call.undefined-method: off`, the
`internal-error` sentinel SURVIVES `apply` and is not re-stamped, while an
ordinary rule under the SAME stamp is dropped (the in-test control — so survival
is the bypass, not an inert override).

Unit-level by necessity, and this is worth stating precisely: **the LSP does not
have a production `internal-error` path at all.** `compute_diagnostics`'s
`catch_unwind` returns an EMPTY diagnostic list on a panicking buffer, whereas
`check` pushes a synthetic `internal-error` finding (`internal_error_diag`). The
bypass is therefore correct-by-construction insurance for the day that path is
added — it is not exercised end to end today. That gap is listed below.

> **Non-vacuity**: replacing `if diag.rule_id == "internal-error"` with
> `if false` ⇒ FAILS at "internal-error survives an `off` config".

### 4. Bleeding-edge — PASS

`lsp_runs_the_bleeding_edge_void_rule_exactly_when_check_does`. Fixture:
`sig/widget.rbs` declaring `def fire: () -> void` plus `lib/void_use.rb` using
that return as a value. **Default** (no config): `check` reports nothing and the
LSP publishes nothing. **`bleeding_edge: [use-of-void-value]`**: `check` reports
`static.value-use.void` at 2:5 `warning`, and the LSP publishes exactly that.

> **Non-vacuity**: `if false && project.stamp.void_rule_active` ⇒ FAILS —
> `left: [] right: [(2, 5, "warning", "static.value-use.void", …)]`.
>
> **The opposite break is NOT detectable, and that is correct**:
> `if true || …` (run the collector unconditionally) still passes, because
> `static.value-use.void` is `:off` in ALL THREE profile tables, so the stamp
> drops it anyway. The gate is a COST optimisation (it is memoised in the
> reference's runner for the same reason); the stamp is what enforces presence.
> Recorded here so a future reader does not mistake the gate for the safety
> mechanism — it is not, and the same is true in `check`.

### 5. S1–S4b preserved — PASS

All 61 pre-slice `lsp::` tests unchanged and green (debounce, 3-axis stale-drop,
single-writer, overlay + its conservative re-harvest, the differential
byte-identity test, guard hysteresis, no-lost-update, panic-never-stuck,
shutdown-no-hang), plus the 5 new stage-3 unit tests ⇒ **66**. The two S4b E2E
parity tests still pass against the stricter (rule-id-carrying) comparison.

## Still divergent between the LSP and `check` after this slice

Stated explicitly, because the previous slice's headline over-claimed and had to
be walked back. Everything below is **known and unfixed**, not "probably fine":

1. **No baseline (ADR-22).** `check` applies `apply_baseline` after the stamp;
   the LSP has no baseline at all, so a diagnostic a project has baselined is
   still published to the editor. Explicitly out of scope for this slice.
2. **Config `exclude:` is not honoured for the OPEN BUFFER.** `cfg.is_excluded`
   filters the overlay's project discovery (`project_files`) but
   `compute_diagnostics` never consults it, so opening an `exclude:`d file
   publishes diagnostics `rigor check` would not produce for it. Pre-existing;
   cheap to close (the matcher is config-derived and would ride on
   `ProjectContext` exactly as the stamp now does), but not this slice.
3. **A panicking buffer produces NOTHING, not `internal-error`.** See acceptance
   3. `check` surfaces the panic as a finding (and on stderr); the LSP silently
   publishes an empty list.
4. **The project root is the process cwd** (S4b's deferred N4): `rootUri` /
   `workspaceFolders` are still not consulted, so a server launched from the
   wrong directory discovers zero project files and — because an empty project
   deliberately does not trip the scale guard — discloses nothing.
5. **`rigor lsp` accepts no `--bleeding-edge` flag.** Only `bleeding_edge:` in
   `.rigor.yml` reaches the editor. Deliberate (an editor launches the server, so
   a per-invocation CLI flag has no user), but it means `check --bleeding-edge=…`
   and the LSP can disagree on the rule set for the same project.
6. **`.rigor.yml` is read ONCE, at startup.** `invalidate` rebuilds the stamp
   from `st.cfg` but never re-parses the config file (matching the reference's
   `ProjectContext#invalidate!`), so editing `severity_profile:` mid-session does
   not move the editor's severities until the server restarts. The stamp is
   *written* to follow `st.cfg` so it comes along for free when a future slice
   makes the config reloadable. Same limitation the `disable:` set has always had.
7. **Buffers outside `paths:`** are analysed against the FULL project index
   (S4b's documented N5 divergence) — better informed than
   `rigor check <that file>`, but not the same answer.
8. **hover / completion / documentSymbol still use the single-file index** (S4b,
   by design: they answer synchronously under a <100 ms p95 budget).

The parity claim after this slice is therefore: **for a saved or dirty buffer
inside `paths:`, in a project with no baseline and no `exclude:` covering that
file, the LSP's published diagnostics equal `rigor check <project>`'s rows for
that file on (rule id, line, column, severity, message).** That is a strictly
larger claim than S4b's (which held only where the resolved severity happened to
equal the authored one), and it is the whole claim.

## Gates

- `cargo build --offline && cargo test --offline`: PASS — workspace green
  (341 + 228 + 157 + 63 + 48 + 41 + 5 + 4 + 3 across the binaries/crates,
  0 failed).
- **5× flake check**: the `lsp::` test binary **66/66 five times, 0 failures**;
  `lsp_check_parity` **5/5 five times**; both **once more under
  `RAYON_NUM_THREADS=1`** (66/66 and 5/5).
- `ruby harness/run.rb` + `ruby harness/run_snapshot.rb`: **PASS, 216/218,
  0 unregistered FP** (unchanged from pre-slice — the LSP is not on the harness's
  path, which is the point of the blast-radius bound).
- `python3 harness/docs_check.py`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings` in a fresh
  `CARGO_TARGET_DIR`: clean.
