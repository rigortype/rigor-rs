# #640 — env-polarity dead-arm judge in the loop-pass stale-write filter

Issue: `c = gets; d = 1; w = 1; y = nil; while c; s = (if d then 2 else
w = "x"; 3 end); y = [y]; end; w.zz` — the reference fires
`undefined method 'zz' for Integer`; the port displayed the literal
(`for 1`), later went silent on master. Same-key message drift the
(rule, line, col) gates cannot see. PR #800, merged `1ebb84ff` (head
`04e0f4ce`).

## What changed

`widen_shell`'s loop-pass stale-write filter (`def_local_marks.rs`,
the #437 filter) folds a tracked name's write positions against
`dead_positions`, which judges reach-pinned predicates — never an env
value-pin inside a loop. The write in the dead `else` arm therefore
spoiled `w`: the pass exit typed it `Dynamic`, the exactness gate
declined the whole fixpoint, and the structural cap widened every
tracked name — where the reference's `BodyFixpoint` cap pass widens
only the still-moving ones (`w` keeps its pin until the cap lifts it
to `Integer`).

- New `env_decided_dead_hulls`: collects the dead-arm hulls of
  `if`/`unless` nodes inside the shell statement whose predicate the
  pass env types as a decided `Constant` — the same Constant-only
  trust base `replay_one`'s If elide uses (`predicate_polarity` for
  the truthiness call; `unless` swaps the arms). A write survives the
  `retain` only if it is outside both judges' hulls.
- `Nominal`/`Tuple` predicate answers do NOT judge — an optimistic
  carrier the reference declines to elide must not excuse a write
  either (probed: `gets.to_i`, `[1]`, `c ? 1 : 2` — the reference
  still fires `for Integer`, the port still declines).
- Filter guard `dead.has_folded()` → `!names.is_empty()`: with no
  folded or env-dead hulls the retain keeps every name, so behaviour
  is unchanged (verified: byte-identical diagnostics on
  markdown/resolv/mkmf/optparse/matrix). Cost: one extra AST scan per
  loop-pass shell statement — 3,000-`if`/1,500-shell synthetic file
  21.2 s → 20.0 s vs master.

## Measured outcome

- Brief row + String twin + `unless` dead-arm: full-tuple parity,
  `for Integer`/`for String` (was `for 1` / silent on master).
- Reviewer re-probed ~90 shapes (method/operator predicates, i64/float
  edges, multi-byte, in-statement predicate reassignment, block
  shadowing, `def`/`until`/interpolation/`case`/nested `while`): every
  port diagnostic matches or declines. No port-only fires, no wrong
  class.
- Disclosed coverage gaps (declines, FP-safe; filed-worthy if the
  loop-display family is pursued): live-arm shell write, undecided
  predicate join, `def`-body/interpolation/call-arg/nested-`while`
  shapes, Nominal/Typed predicates.
- Reference does NOT elide `!d`, `not d`, `d.!`, `d.is_a?` but DOES
  elide `d.nil?`, `d == 1`, `d > 0`, `d.zero?`, `d.positive?` —
  recorded on the PR as the boundary for any future widening of the
  trust base.
- Sweep: 0 FP / 437 gaps = master. **0 sweep gaps closed** — a
  message-drift parity fix no sweep row covers; merged under the
  owner's standing discretion for 0-gaps slices.
