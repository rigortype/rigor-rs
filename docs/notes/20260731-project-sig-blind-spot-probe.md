# Probing the project-`sig/` blind spot (2026-07-31)

`harness/fp_audit.py` and `harness/run_corpus.rb` both run each side from a clean
cwd — core+stdlib only, no project config — so **no project-`sig/` behaviour is
measured by either**, at any corpus size. 9204 swept files say nothing about it.
The only coverage is staged fixtures (37/38 project sig, 39 rbs collection,
69/70 nested), which is a handful of shapes.

So: build a matrix of project shapes by hand and run both engines in each. Seven
probes, three findings — including one **latent false positive** against the
pinned oracle that no sweep could ever have surfaced.

## The matrix

Each probe is a project (`lib/*.rb` + `sig/*.rbs`), both engines invoked from
its root so each auto-loads `sig/`. Reference = the pinned `v0.3.1`.

| # | shape | reference (pin) | rigor-rs | verdict |
|---|---|---|---|---|
| 1 | `sig/` references an undeclared INTERFACE / type ALIAS | **silent** (whole stub batch discarded) | fires | **excused divergence — see below** |
| 2 | `sig/` declares a method the source lacks | fires on the undeclared one only | same | agree |
| 3 | class exists only in `sig/`, never in source | fires on the typo | same | agree |
| 4 | `sig/` has an RBS **syntax error** | reports the parse error + says coverage is reduced | **silent** | gap (below) |
| 5 | `sig/` is **invalid UTF-8** | `internal analyzer error: ArgumentError` | silent, analyses fine | reference defect, fixed upstream |
| 6 | `sig/` reopens a CORE class | fires on the non-declared method | same | agree |
| 7 | nested namespace declared in `sig/`, called unqualified from inside the module | **fires** `Outer::Inner#absent_zzz` | **silent** | coverage gap |

## Finding 1 — a latent FP against the pin, now gated (fixture 79)

Probe 1 is rigor-rs emitting a diagnostic the pinned oracle does not: exactly
what the zero-FP bar forbids. It survived unseen because it needs a project
`sig/` to reproduce.

The mechanism is upstream's, not ours: Rigor stubs a referenced-but-undeclared
type so RBS's all-or-nothing per-class build still succeeds, but an interface or
alias name **cannot be declared as a `class`**, so one such stub makes the whole
batch unparseable and the pinned reference discards it **in silence** — a project
whose missing names are all aliases gets nothing at all from its own `sig/`.
rigor-rs declares each stub in the kind its name requires, keeps the signature,
and witnesses the typo.

Upstream agrees it was a defect and fixed it the same way after `v0.3.1`
([#237](https://github.com/rigortype/rigor/issues/237), `9515c8f8` + `5bd0aac2`).
That makes it a textbook [ADR-0011](../adr/0011-reference-oracle-exceptions.md)
entry rather than a bug to "fix" by making the port worse: **fixture 79** pins
the shape and `harness/divergence-registry.yml` excuses the extra, with the
instruction to delete the entry when the pin passes the fix. The registry was
empty before this; it is designed to trend back to empty.

## Finding 2 — a broken `sig/` file is silently ignored (probe 4)

Given `def ok: () ->` (truncated), the reference reports the parse error with its
file and position and warns that signature coverage is reduced. rigor-rs says
**nothing** and analyses as if the file were absent — the user's signatures
silently do not apply. That is not a diagnostic-set parity break (the reference's
output here is a warning block, not a rule diagnostic), which is precisely why no
gate catches it, and it is a bad failure mode for a tool whose value is the
signatures.

Not fixed here: it wants its own slice (where the message goes, whether it is a
rule or a stderr note, and what the exit code should be), and this session has
three agents in flight touching the index and CLI.

## Finding 3 — nested-namespace resolution from inside the module (probe 7)

```ruby
module Outer
  class Inner; def run = 1; end
  def self.go
    Inner.new.absent_zzz   # reference fires, rigor-rs silent
  end
end
```

with `Outer::Inner` declared in `sig/`. Fixtures 69/70 cover the *qualified*
nested-sig shapes; this is the unqualified reference from inside the enclosing
module, and it is a plain coverage gap (FP-safe). Also left for its own slice —
it lives in the same qualified-key resolution the ADR-0042 migration owns.

## What to do with this

The probe matrix is cheap to re-run and belongs in the loop after any
project-sig, scoping, or resolution change. The lesson worth keeping is the shape
of the blind spot rather than any one finding: **a green sweep over four figures
of real files is not evidence about a surface the sweep does not exercise.**
