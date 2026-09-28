# #164 — literal-argument nilable lookup folds

PR #283 (merged, head `d42a8e6`). A literal-argument call to a nilable core
lookup whose fold lands on `nil` used to answer the flat `Integer` slot and
fire `call.undefined-method` where the reference is silent (an FP), and on hits
it said `for Integer` where the reference says `for 1`.

`folding.rs`: `fold_str_lookup` gained `getbyte` (negative index from the end),
`rindex`/`byterindex` (start offset: default end, negative from end, clamped
past end; last match at-or-before start) and `byteindex` (reuses `index`). The
ASCII gate now covers the needle too. `fold_float` gained `<=>` via `float_cmp`
(exact Integer compare; String/Symbol/Bool/Nil → nil; NaN/inf declines — the
reference never pins a non-finite Constant). `call_dispatch.rs`: `stale_risk`
widened to `is_nilable_fold`; `declines_unfolded` answers Dynamic rather than
the bare `Integer` when one of the five methods has pinned args but the fold
declines (the reference rescues a raise into `Integer | nil`, which fires
nothing — probed identical).

Measured: gate.sh 0, CI green, sweep 0 FP / 9,337; fixture 119. All brief rows
match incl. message text (`for nil`, `for 1`, `for 97`, `for -1`); the stale
local `x << "d"; x.getbyte(3)` stays declined.

Residual coverage losses (disclosed, zero FP): multibyte receivers
(`"é".getbyte(0)`), Float index, env-local receiver/arg under the stale guard,
chains past a nil fold. Adjacent: `"abc".rindex(/z/)` is #178's Regexp row;
`[]`/`slice`/`byteslice`/`index` still fall to tier 3 on a stale-local decline
(only the five new methods get the Dynamic guard).
