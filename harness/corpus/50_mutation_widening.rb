# MutationWidening: a value-pinned collection local that is content-mutated by an
# in-place mutator call (`push`/`pop`/`<<`/…) must WIDEN, so a later
# `results.count`/`.size` predicate no longer folds to a constant and
# flow.always-truthy-condition stays SILENT. This is the reference's
# MutationWidening subsystem (widen_after_call + widen_after_block); the measured
# FP was gitlab-foss lib/gitlab/ci/pipeline/expression/parser.rb:41-42, where
# `results = []` is push/pop-mutated inside an `each` block and `results.count > 1`
# / `< 1` wrongly folded on rigor-rs while the reference was silent.
#
# The NEGATIVE CONTROLS (no mutation) at the bottom MUST still fire on BOTH tools —
# widening an unmutated local would be the opposite bug.

# --- SILENT: mutations widen the local (no always-truthy fire) ---

# Straight-line push (no block): `results.push(1)` widens `results`.
sa = []
sa.push(1)
if sa.count > 1
  puts "sa"
end

# push under an `if` modifier: the mutated then-branch disagrees at the join.
sb = []
sb.push(1) if guard
if sb.count > 1
  puts "sb"
end

# The parser.rb shape — push/pop inside a nested `case` in an `each` block,
# `> 1` direction.
sc = []
tokens.each do |token|
  case token
  when :value
    sc.push(token)
  when :unary
    sc.pop
  end
end
if sc.count > 1
  puts "sc"
end

# Same block shape, `< 1` direction (parser.rb line 42).
sd = []
tokens.each do |token|
  case token
  when :value
    sd.push(token)
  end
end
if sd.count < 1
  puts "sd"
end

# `<<` (shovel) inside a block.
se = []
items.each do |x|
  se << x
end
if se.count > 1
  puts "se"
end

# --- NEGATIVE CONTROLS: NO mutation, always-truthy MUST fire on both tools ---

# `nc1` stays `[]`; `[].count` folds to `0`, `0 > 1` -> false -> always falsey.
nc1 = []
if nc1.count > 1
  puts "nc1"
end

# Same, `< 1` direction: `0 < 1` -> true -> always truthy.
nc2 = []
if nc2.count < 1
  puts "nc2"
end
