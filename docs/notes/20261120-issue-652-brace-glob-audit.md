# #652 — brace-spelled globs flag like star globs in the analysis-stack audit (PR #719)

Filed by the Grok 4.6 advisory review of #601: the audit flagged
`use rigor_parse::*;` / `use std::thread::*;` as `use:alias-or-glob`,
but the same glob written with braces — `use rigor_parse::{*};` — was
silent, so `parse(src)` / `spawn(f)` call sites escaped every pattern
through a spelling rustfmt would have collapsed anyway (`cargo fmt
--check` is intentionally not a gate).

## The fix

`after_ident_is_glob` required `ident :: *` with only whitespace in
between. After `::` it now also accepts a `{` group:
`brace_group_has_self_glob` walks the group and globs the ident iff a
`*` list item sits at the group's own depth and is not immediately
preceded by `::` (`preceded_by_colon_colon`, whitespace skipped
backwards). Nested child globs (`::{a::*}`, `::{a::{*}}`) are not a
glob of the *parent* ident and stay unflagged — same leaf semantics as
the unbraced walk. Mixed lists (`::{a, *}`, `::{*, a}`) flag; a mixed
`use std::thread::{spawn, *}` keeps the `use:spawn` leaf tag too.

Test-only: `analysis_stack_audit` is a `#[cfg(test)]` module, so no
`check` diagnostic moves.

## Measured (all on head `b4d8c22a`)

- Trip-wire test `brace_glob_uses_flag_like_star_globs`: 23 matcher
  rows + full-scan rows over both spellings, mixed lists, the issue's
  two planted shapes, and negatives (`{a::*}`, `{a::{*}}`,
  `use std::process::{*}`).
- Suite: 635 unit tests green in a fresh worktree, including the
  production scan — the extended matcher flags nothing new in the
  repo's own source.
- Snapshot: 174 fixtures, 980 matched, 0 unregistered FP. Sweep
  (release): 8/8 corpora, 0 FP candidates.
- rustc grammar probes on 1.100.0 AND the pinned 1.88.0: `use
  std::{{*}};` compiles (unused-import warning only) and globs —
  anonymous nesting is real, disclosed in the matcher's doc comment,
  closed by the #654 flattener. `self::*` spellings (`::{self::*}`,
  `::{self::{*}}`, unbraced `::self::*`) compile and glob the parent
  only on 1.100; on the pinned 1.88 all three fail E0433, so the gap
  is latent until the pin moves — pre-existing for the unbraced walk.
  Filed to #654 with a hardening note: `close_of` panics on an
  unbalanced use brace in a stray non-module file (unreachable on
  compiling files; a flattener should walk to end-of-span with
  saturating depth instead).

## Review

Adversarial pass on the head: 11 body claims re-probed, all held;
4 non-blocking findings (the two above + two confirmed-correct
declines), all filed/disclosed. Approved on the first round; Fable 5.1
design companion unavailable (account quota), and no repeated
Needs-fix family existed to remediate.
