# `call.wrong-arity` declines on ANY non-plain-positional argument — a
# `*splat`, a bare keyword-hash `a: 1`, or forwarded `...` — mirroring the
# reference's `plain_positional_call?` / `simple_positional?`
# (`check_rules.rb:1680`; issue #165).
#
# The port used to count every `*a` as one positional and fired
# "given 2" on `first(*[5], *[5])` where the reference skips the arity
# check entirely. A `&blk` block-pass rides Prism's `block()`, not
# `arguments()`, so it does NOT disqualify: `first(1, 2, &)` still fires
# on the oracle (kept here as a firing parity row), while
# `first(1, 2, &b)` fires on the reference but stays a port coverage gap
# (the port's single-envelope `has_block` deferral — declining is the
# safe side, and the gap predates this fix).
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, a fresh temp cwd per probe, `--no-cache` on the
# reference, both reference libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- (1) SILENT: splat argument shapes — the issue table ---------------------

[1, 2].first(*[5], *[5])
w = [5]
[1, 2].first(*w, *w)
def splat_in_def(s) = [1, 2].first(*s, *s)
[1, 2].first(*[5], 1)
[1, 2].first(1, *[5])
# Zero args at runtime — correct code the port used to flag as "given 2".
[1, 2].first(*[], *[])
"abc".center(*[5], *[5], *[5])
# A lone splat was already silent on both; kept as a control.
[1, 2].first(*[5])
# The gate does not depend on `.` vs `&.`.
[1, 2]&.first(1, *[5])
nil&.first(1, *[5])

# --- (2) SILENT: the other `simple_positional?` declines ---------------------

# A bare keyword-hash (Prism's KeywordHashNode) is not a plain positional.
[1, 2].first(1, a: 2)
[1, 2].first(1, 2, a: 3)
# `...` argument forwarding.
def fwd(...) = [1, 2].first(1, ...)

# --- (3) Block-pass: `&` fires on both, `&b` is a port coverage gap ----------

# Prism puts `&`/`&b` in `block()`, never in `arguments()`, so the
# reference's argument list stays all-plain and it arity-checks. The
# anonymous `&` lowers no block expression and keeps firing on both;
# `&b` is silent on the port only — a pre-existing `has_block` coverage
# gap, expected to show as a missing row here.
def anon_block(&) = [1, 2].first(1, 2, &)
def named_block(&b) = [1, 2].first(1, 2, &b)

# --- (4) FIRES: plain-positional controls -------------------------------------

[1, 2].first(1, 2)
"abc".center(1, 2, 3)
