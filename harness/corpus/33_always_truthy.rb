# `flow.always-truthy-condition` fires on an if/unless/ternary whose predicate
# the dominating flow scope folds to a `Type::Constant` (the inferred-constant
# counterpart to the syntactic-literal `flow.unreachable-branch`). Byte-for-byte
# against the oracle on (rule, line, column = the predicate node). Each case
# below binds a local to a constant on a STRAIGHT-LINE (dominating) write, so the
# predicate folds soundly — the flow scope's branch-join keeps the constant only
# because nothing reassigns it conditionally (see 34 for the decline cases).

# A literal-assigned constant: `ca` folds to `5` -> always truthy.
ca = 5
if ca
  puts "a"
end

# A `nil` constant -> always falsey (in Ruby only nil/false are falsey).
cb = nil
if cb
  puts "b"
end

# An INFERRED constant (folded arithmetic, not a syntactic literal): `cc` folds
# to `2` -> always truthy. This is the case the syntactic `unreachable-branch`
# rule cannot reach (its predicate is not a literal node).
cc = 1 + 1
if cc
  puts "c"
end

# The `unless` keyword: predicate `cd` folds to `false` -> always falsey
# (the polarity is the predicate VALUE, independent of which branch runs).
cd = false
unless cd
  puts "d"
end

# A PARENTHESES-wrapped predicate anchors on the `(` column, not the
# unwrapped node: `ce` sits at col 5 but the diagnostic lands at col 4.
ce = 5
if (ce)
  puts "e"
end

# Nested parens anchor on the OUTERMOST `(`. A compound predicate inside
# parens anchors the same way.
cf = 5
if ((cf))
  puts "f"
end

cg = 5
if (cg + 1)
  puts "g"
end

# Ternary and modifier-if predicates anchor identically.
ch = true
(ch) ? (puts "h") : nil

ci = 5
puts "i" if (ci)
