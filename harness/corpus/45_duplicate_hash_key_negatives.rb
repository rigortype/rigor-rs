# `flow.duplicate-hash-key` decline cases — each hash below has NO colliding
# value-pinned literal key, so the rule stays silent (zero-FP envelope).

# Distinct kinds never collide: string vs symbol, integer vs float.
a = { "k" => 1, k: 2 }
b = { 1 => "x", 1.0 => "y" }

# Computed / interpolated keys are never value-pinned.
c = { foo => 1, foo => 2 }
d = { "#{prefix}" => 1, "#{prefix}" => 2 }

# Distinct literal keys — the ordinary case.
e = { first: 1, second: 2, third: 3 }

# A `**splat` alone is not a key.
f = { **defaults, only: 1 }
