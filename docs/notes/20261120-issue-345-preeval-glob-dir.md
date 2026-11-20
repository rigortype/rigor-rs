# #345 — a pre_eval glob matching a directory earns `pre-eval.file-not-found` (PR #720)

Filed as a residual of the #158 config arc: the reference expands every
`pre_eval:` entry at config load (`Configuration#expand_pre_eval_entries`
— `Dir.glob` / `RootedGlob.glob`, sorted, `.uniq`) and
`DiagnosticAggregator#pre_eval_diagnostics` runs `File.file?` over that
EXPANDED stream, so a glob whose matches include a directory produces a
not-found row for that match. The port's run-level pass skipped every
glob-meta entry outright (with a comment claiming they never earn the
row), so `pre_eval: ["setup*"]` over `setup.d/` was silent where the
reference exits 1.

## The fix

`main.rs`'s inline loop became `pre_eval_not_found_rows`: each raw entry
is expanded via `pre_eval::expand_entry_for_rows`, the `seen` dedupe keys
the EXPANDED matches (mirroring `.uniq` — a glob and the same path
spelled literally earn ONE row), `File.file?` gates each match, and the
stale comment is gone. `pre_eval.rs`'s `expand_entry` split into
`expand_entry_with(options)`: the REGISTRY keeps its superset options
(an extra match only makes it quieter — safe); the ROW side matches
case-sensitively and never matches dot-leading names, because an extra
non-file match would be an FP row. Brace groups still return `None`
(declined) — the port cannot reproduce `Dir.glob` brace alternatives.

Two measured `Dir.glob` behaviours (ruby 4.0.5) pin the row-side
options: a leading `.` only matches literally (`*.rb` skips
`.hidden.rb`), and case folding follows the VOLUME — `a*` matches
`A.rb` on the default case-insensitive APFS volume and nothing on a
case-sensitive one (`File.fnmatch` case-sensitive on both; probed on
both volume kinds). Case-sensitive is therefore exact on CI's Linux and
only ever declines on a default macOS volume.

## Measured (all on head `f51277d8`)

- Same-cwd probes (both engines, full stdout + exit diffed,
  byte-identical): the brief's row; file+dir (row for the dir only);
  matchless glob; literal dir; glob+literal dedupe in both orders; two
  globs in config order; rooted `lib/*.rb` over a directory spelled
  `b.rb`; `*` with a `.git/` present; `ds [x]/patch.rb` missing (a
  parity gain — the old skip lost rooted literals too); `lib/**` at one
  level.
- Declines (coverage gaps, safe side): `SETUP*` on a case-insensitive
  volume; `{a,b}` braces.
- `gate.sh` pass; CI 5/5; sweep (release) 0 FP / 8 corpora. Unit
  tests: 4 in `tests.rs` + 1 stance pin in `pre_eval.rs`.

## Review

Opus 5.5 adversarial pass on `f51277d8`: ~30 counterexample shapes
re-probed identical (symlinks, dotfiles, `**` levels, dedupe, `disable:`,
severity, `--format json`); Approved on the first round with four
NON-BLOCKING glob-dialect FPs and two pre-existing master divergences,
all filed: #721 (the `glob` crate is a different DIALECT, not a subset —
a bare trailing `**` recurses, `[^…]` reads literally, `**/` follows
symlinked dirs, `\` is a literal; cheapest safe fixes named there) and
#722 (rows hard-code `.rigor.yml` where the reference uses
`config_display_path` — the carried "aggregator hard-codes it" comment
is stale at this pin — and `{:?}` escapes combining marks Ruby's
`inspect` prints raw). The Fable design companion was quota-blocked on
both available instances; the recorded design stance is the two-site
split — one shared config-load expansion cannot serve both sides of the
zero-FP bar (registry superset, rows subset).
