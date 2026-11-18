# Issue #490 — singleton-slot writers + `severity_overrides` key naming (PR #602)

Merged 2026-11-18 (`9c0b8fc`). Three review rounds; every finding re-probed
on both engines (`harness/probe.py`, full tuple + exit code).

## What landed

**Singleton-slot events** (`source_index/literal_fold.rs`) — the port's
`SingletonEvent` scan now sees the writer set `scope_indexer.rb`'s
`SingletonDefTable` sees:

- `def self.x` inside `class <<` files on the OWNER (`def_singleton?` reads
  only the SelfNode receiver). Inside `class <<` EVERY receiver def files on
  the owner — `in_singleton_class` is unconditional (`def W.x`, `def obj.x`).
  In a direct body `def V.x` must match the WRITTEN-name chain
  (`def_receiver_targets_lexical_self?`): `module A::V { def V.x }` files
  nothing — a `::`-split tail-match was round-1's headline FP.
- Eval blocks on a constant receiver: bare/`self.` `define_singleton_method`,
  `singleton_class.<naming>` calls, and `alias` under `instance_*` contest
  the receiver's slot; a `class <<` nested in the block makes bare defs
  bind. `W.instance_eval` inside `module V` files on W, not V — the
  unconditional foreign `def <path>.x` event is gone.
- The governing context of a def/call is the INNERMOST of {`class <<`,
  eval}: `class_eval { class << self { def x } }` writes; `class << self {
  V.class_eval { def x } }` doesn't.
- `class <<` interiors outside a class body's scan (top level, inside
  `class << Const`, inside evals) supersede the target's slots via naming
  calls + `alias`; `Const.singleton_class.X` inside `class <<` stays quiet
  (`!in_singleton_class`).

**`severity_overrides` key naming** (`rigor-cli/src/config.rs`) —
`psych_key_to_s` mirrors `ToRuby#deserialize`: `str` reads the value,
`float` does `Float(tokenize)` (`!!float 1e3`→`"1000.0"`, `.inf`→
`"Infinity"`, `0x10`→`"16.0"`), every other tag tokenizes the plain text
(`!!int 0o17`→`"0o17"`, `!!bool 0x10`→`"16"`). The line scan sees `!!name`,
`!name`, verbatim `!<tag:…>`, quoted and flow `severity_overrides:` keys,
takes the smallest-indent block, reads only direct key lines, and counts
tags only in key position. Duplicate top-level blocks → last only;
spellings disagreeing on one loaded key warn every settled name
(`0o17`+`15`).

## Review history (the interesting part)

- Round 1 (Opus): 4 blockers — `module A::V` written-name match, `15`→`"15.0"`
  (`as_f64` accepts ints), `<<:` seq aborting the spelling scan, nested
  `severity_overrides:` leaking into the tag scan.
- Round 2 (Opus): all landed; Needs fix on **clippy 1.88 only** + one new FP
  (`class_eval { class << self { def x } }`).
- Fable design companion: the `class <<`-composition FP family (f1–f6:
  naming calls/`alias` inside `class <<` outside class bodies) and the
  tag-scan edges. Its structural advice stands: port
  `walk_singleton_def_nodes` + `SingletonDefTable` + `Certainty` as one
  recursive walk rather than four span-enumerators, and read the YAML event
  stream once instead of three text passes — both deferred as design
  follow-ups, not gaps in this slice's contract.
- Round 3 (Opus): **Approved**, ~65-row regression sweep clean.

## Residuals (disclosed; all safe-side or pre-existing)

- Last-writer-wins declines: `SingletonDefTable` folds the LATER write, the
  port declines on disagreement. Closes for free with the table port.
- `undef` keyword not lowered → needs a `Node::Undef` (Fable calls this the
  next PR's prerequisite).
- Filed: **#606** (`class << Const; def x` in non-constant-receiver blocks —
  pre-existing FP), **#607** (sequence keys under `severity_overrides`),
  **#608** (nested `class << self` coverage regression vs master).
- `fp_audit --gaps --sweep` NOT RUN — no corpora on the machine; disclosed.
- Loader divergences out of scope: serde_yaml rejects `!!float 0x10`,
  `!!bool yes`, `!!null x` where Psych loads them.
