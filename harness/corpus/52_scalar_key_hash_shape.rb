# Scalar-key HashShape (v0.3.0, ADR-0038 slice 2). Hash literals now pin
# Integer / Float / true / false / nil keys (not only Symbol / String), a
# duplicate key is last-wins (first position, last value), and a HashShape
# projection tier folds static-key reads. Each receiver renders exactly as the
# oracle renders it, and a folded projection result witnesses a chained
# undefined method byte-for-byte on (rule, line, column, message).

# Integer-keyed hash: the receiver renders with hashrockets (`{ 1 => 2, 3 => 4 }`).
a = { 1 => 2, 3 => 4 }
a.frobnicate

# Float / true / false / nil keys all pin now; Symbol/String would keep the
# colon form, these use the hashrocket.
b = { 1.5 => :x, true => 1, false => 2, nil => 3 }
b.frobnicate

# Mixed keys in one literal: `a:` and `"k":` keep the colon form, `7 =>` the
# hashrocket.
c = { a: 1, "k" => 2, 7 => 3 }
c.frobnicate

# A duplicate key is last-wins: `1` keeps its first position but takes the last
# value, so the receiver renders the collapsed `{ 1 => 9 }`. Ruby also warns on
# the duplicate, so `flow.duplicate-hash-key` fires alongside — both matched
# against the oracle.
d = { 1 => 1, 1 => 9 }
d.frobnicate

# --- projection tier: a static-key read folds to the precise member type ---

# `h[:key]` folds to the value type, so the chained undef witnesses `for "s"`.
e = { a: 1, b: "s" }[:b]
e.frobnicate

# `h.fetch(:key)` folds identically (a present key never raises); `for 42`.
f = { a: 42 }.fetch(:a)
f.frobnicate

# `h.has_key?(:key)` folds to a precise bool; the chained undef flags `for true`.
g = { a: 1 }.has_key?(:a)
g.frobnicate

# An Integer-key read folds too, witnessing `for "x"`.
h = { 1 => "x" }[1]
h.frobnicate
