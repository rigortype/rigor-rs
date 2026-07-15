# `flow.duplicate-hash-key` fires on the LATER occurrence of a repeated
# value-pinned literal key within one Hash literal (braced or bare kwargs).
# Ruby keeps the last entry silently at runtime, so the earlier value is dead.
# Byte-for-byte against the oracle on (rule, line, column = the repeat key node).

# Symbol shorthand — the canonical `:name` label, first-set line named.
a = { role: :admin, role: :user }

# Hashrocket string — the label is the Ruby `String#inspect` of the key.
b = { "env" => "prod", "env" => "dev" }

# Same integer value collides; distinct-kind int vs float never does (see 45).
c = { 1 => "x", 1 => "y" }

# Same float — `1.0` and `1.00` are the same f64, label is the verbatim slice.
d = { 1.0 => :a, 1.00 => :b }

# `true` / `false` / `nil` literal keys.
e = { nil => 1, nil => 2 }

# A `**splat` between two identical keys does NOT rescue the collision.
f = { **base, size: 1, size: 2 }

# Bare keyword arguments (`m(a: 1, a: 2)`) are scanned too.
configure(timeout: 30, timeout: 60)

# Three identical keys fire TWICE, both naming the ORIGINAL first occurrence.
g = { k: 1, k: 2, k: 3 }

# A nested literal is its own scope: only the inner `x:` pair fires.
h = { x: 1, y: { x: 2, x: 3 } }
