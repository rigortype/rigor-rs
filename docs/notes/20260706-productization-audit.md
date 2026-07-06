# Productization-arc audit — ADR-0040/0041 decisions (2026-07-06)

> **Status: absorbed.** Findings #1–#3 were fixed the same day (exit-code
> internal-error exception, `baseline generate` config-paths default, symlinked
> `.rb` files in the dir walk); ADR-0040 was corrected/amended accordingly
> (#3 wording, #4 known-limitation, #1 amendment). #5 remains a standing note.
> This file is the review record.

An audit of the session arc from the directory-support design through the
flow-frontier pivot and the config-`paths:` landing. All findings were
probe-verified against the oracle / code before being reported. Ranked.

## 1. (REAL BUG — fixed) panicked analysis exited 0 under the new exit rule

`internal_error_diag` is deliberately `Severity::Info` (to stay out of the
differential harness's error/warning parity gate — a valid reason). ADR-0040's
error-severity-driven exit code (`exit 1 iff any ERROR finding`) therefore let a
run whose analysis PANICKED exit 0 — CI silently green on a crashed file. The
old "exit 1 on any finding" rule had covered panics by accident; the severity
rewrite dropped that cover. **Fix:** `finding_fails_run` — ERROR severity OR
`rule_id == "internal-error"` fails the run; severity stays Info (harness
reason intact). Unit-tested.

## 2. (reference mismatch — fixed) `baseline generate` didn't default to config `paths:`

Probed: the reference's bare `baseline generate` writes a baseline from
`configuration.paths` (it generated from `lib/` with no args); rigor-rs errored
`expected at least one file`. Inconsistent with the just-landed bare-`check`
behavior and with the "unblocks baseline subcommands" claim. **Fix:** same
roots logic as `check` (bare ⇒ `cfg.paths`, explicit args override).
E2E-verified identical to the reference.

## 3. (faithfulness — fixed) the dir walk skipped symlinked `.rb` FILES

Probed: Ruby's `Dir.glob("**/*.rb")` **matches symlinked files** (it only
declines to traverse symlinked DIRECTORIES). The first-cut walk skipped both,
losing coverage (and diverging from the reference's diagnostic set on any repo
with a symlinked `.rb`). ADR-0040's "matches Dir.glob" claim was inaccurate for
files. **Fix:** classify a symlink by its target — file ⇒ match, dir ⇒ skip.
E2E parity verified (`link.rb` + `real.rb` both reported by both tools).
Unit-tested (incl. no double-traversal via a symlinked dir).

## 4. (edge divergence — documented, not fixed) `paths:` base is CWD

The reference resolves relative `paths:` against the project root (the config
base); rigor-rs uses the process CWD. Coincide in the normal auto-discovery
case; diverge only under `--config elsewhere/.rigor.yml`. Recorded as a known
limitation in ADR-0040; revisit if that workflow materialises.

## 5. (process risk — standing) piece A code lives on a LOCAL branch only

ADR-0041 on master points at branch `tier-bc-nilable-return` for the preserved
piece-A implementation. The branch is local; if it is deleted the record's
backing code is gone (recoverable only via reflog until GC). Push the branch to
the remote when one exists, or accept the ADR text as the durable record.

## Verified sound (no action)

- Bare `check` with a missing `lib/` (default paths): reference-identical
  (error + exit 1) — probed.
- The piece-A revert discipline is consistent: the ADR-0038 substrate landing
  was a ONE-TIME documented §5 exception; piece A correctly got no second
  exception. Master carries zero piece-A code (grep + 438 tests green).
- The flow-frontier conclusion ("no cheap FP-safe wins left") is data-backed
  (three slices, valid-mode gap classification) and falsifiable ("no speculative
  flow slice without an `fp_audit --gaps` prediction").
- The session's measurement-artifact corrections (dir-mode no-op, reference
  cache pollution, shell-quoting) are all honestly recorded in the docs.
- Explicit-args-over-config precedence, path-error severity/message/placement,
  and the warn-and-skip exit-0 semantics: all probe-verified reference-identical.
