# rigor-rs#943 — element-read widening (`ElementReadWidening.
# widen_element_read`) through every element-read chain, not only a single
# literal `[]` step: a mutator whose receiver is a chain of `[]` / `first` /
# `last` / `dig` reads rooted at a local rebuilds that local's literal-shape
# carrier through the whole path. Each `.w` probe reads the rebuilt carrier.

# (1) chained `[]` reads.
b1 = [[[]]]
b1[0][0] << 1
b1.w

h1 = { a: [[1]] }
h1[:a][0] << 2
h1.w

# (2) `first` / `last` / `dig` steps.
a2 = [[1], [2]]
a2.last << 3
a2.w

a3 = [[1]]
a3.first << 1
a3.w

h2 = { a: [[]] }
h2.dig(:a, 0) << 1
h2.w

# (3) a non-literal key selects every position (`:all`).
h3 = { a: [], b: "s" }
k3 = :a
h3[k3] << 1
h3.w

# (4) a block on the MUTATOR still widens.
a4 = [[1]]
a4[0].map! { |i| i.to_s }
a4.w

# (5) a position names no hash key: the shape stands.
h5 = { a: [] }
h5[1] << 1
h5.w

# (6) must stay silent — a rescue modifier's body may raise before the
# mutation, and the reference's rescue join answers no literal.
b6 = [[[]]]
b6[0][0] << 1 rescue nil
b6.w

h6 = { a: [] }
h6[:a] << 1 rescue nil
h6.w

# (7) must stay silent — an undecided arm whose rebuild would change the
# carrier.
b7 = [[1]]
if rand > 0
  b7[0] << 2
end
b7.w

b8 = [[[]]]
if rand > 0
  b8[0][0] << 1
end
b8.w

# (8) a block body's `break` argument: the reference widens with no
# argument types; the port declines rather than keep the stale literal.
b9 = [[[]]]
[1].each { break b9[0] << 1 }
b9.w
