# Kernel `#p` / `#pp` identity typing (v0.3.0, ADR-0038 inference cluster). `p`
# and `pp` return their argument verbatim: one argument comes back unchanged
# (pins/shapes preserved), several come back as an Array of them, and zero
# returns nil. The folded value flows into a chained call so an undefined method
# on it is witnessed exactly where the oracle witnesses it — byte-for-byte on
# (rule, line, column, message). The fold fires for an implicit-self `p`/`pp`
# call that passes the guards, AND for the explicit `Kernel.` module_function
# spelling (`Kernel.p` dispatches to the same intrinsic — upstream c9d2e473).

# 1-arg identity: the Integer pin passes through, so the chained call flags
# `for 42` (Integer has no `frobnicate`).
a = p 42
a.frobnicate

# `pp` behaves identically to `p`.
b = pp 42
b.frobnicate

# N args → a Tuple of the arg types; the receiver renders `[1, "a"]`.
c = p(1, "a")
c.frobnicate

# 0 args → nil; the chained call flags `for nil`.
d = p
d.frobnicate

# A block does NOT block the fold — `p(x) { ... }` still returns `x`.
e = p(42) { 1 }
e.frobnicate

# A HashShape argument passes through the identity unchanged (`for { a: 1 }`).
f = p({ a: 1 })
f.frobnicate

# Explicit `Kernel.` receiver: `module_function` exposes each intrinsic as a
# public singleton on the Kernel module object, so `Kernel.p(42)` dispatches to
# the same identity fold as the implicit-self spelling and witnesses `for 42`.
g = Kernel.p(42)
g.frobnicate

# --- silent directions (fold declines → Dynamic receiver → no witness) ---

# A splat argument makes the positional arity (identity-vs-Array) statically
# unknown, so the fold declines and the result stays Dynamic (silent).
arr = [1, 2]
h = p(*arr)
h.frobnicate
