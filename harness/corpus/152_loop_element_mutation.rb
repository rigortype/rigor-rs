# Upstream v0.4.7 (#2395): a loop over a blockless element-exposing call
# (`values`, `dup`, `clone`, `each`, `map`, `each_with_index`,
# `with_index`) is handed the receiver's own elements, so a body that
# mutates an element widens the receiver's local. A mutator on an element
# read rooted at a captured local (`[0].each { |i| a[i] << 2 }`,
# `loop { b[0] << 2; break }`), and a straight-line `c[0] << 2`, widen it
# too.
#
# Must-still-fire controls: a read-only loop, a mutation of a copy
# (`a.dup << [2]`), `values_at` (not an exposing call), a block-local
# copy, a shadowing block parameter.

def sources152
  h = { a: [1] }
  h.values.each { |v| v << 2 }
  puts 1 if h[:a].size == 1
  a = [[1]]
  a.dup.each { |v| v << 2 }
  puts 2 if a[0].size == 1
  b = [[1]]
  b.clone.each { |v| v.push(2) }
  puts 3 if b[0].size == 1
  c = [[1]]
  c.each.each { |v| v << 2 }
  puts 4 if c[0].size == 1
  e = [[1]]
  e.each_with_index.each { |v, i| v << i }
  puts 5 if e[0].size == 1
  f = [[1]]
  f.each.with_index.each { |v, i| v << i }
  puts 6 if f[0].size == 1
  k = [[1]]
  for v in k.dup
    v << 2
  end
  puts 7 if k[0].size == 1
  r = [[1]]
  r.dup.each(&:clear)
  puts 8 if r[0].size == 1
end

def outer152
  a = [[1]]
  [0].each { |i| a[i] << 2 }
  puts 11 if a[0].size == 1
  b = [[1]]
  loop { b[0] << 2; break }
  puts 12 if b[0].size == 1
  h = { k: [1] }
  [:k].each { |k| h[k].push(3) }
  puts 13 if h[:k].size == 1
  c = { k: { j: [1] } }
  2.times { c[:k][:j] << 1 }
  puts 14 if c[:k][:j].size == 1
  e = [[1]]
  [1].each { e.first << 2 }
  puts 15 if e[0].size == 1
  s = [[1]]
  s[0] << 2
  puts 16 if s[0].size == 1
end

def controls152
  h = { a: [1] }
  h.values.each { |v| v.size }
  puts 21 if h[:a].size == 1
  a = [[1]]
  a.dup << [2]
  puts 22 if a[0].size == 1
  b = [[1]]
  b.values_at(0).each { |v| v << 2 }
  puts 23 if b[0].size == 1
  c = [[1]]
  [0].each { |i| x = [c[i]]; x[0] = 1 }
  puts 24 if c[0].size == 1
  d = [[1]]
  [0].each { |d| d << 1 }
  puts 25 if d[0].size == 1
end

t = { a: [1] }
t.values.each { |v| v << 2 }
puts 31 if t[:a].size == 1
u = [[1]]
[0].each { |i| u[i] << 2 }
puts 32 if u[0].size == 1
w = { a: [1] }
w.values.each { |v| v.size }
puts 33 if w[:a].size == 1
