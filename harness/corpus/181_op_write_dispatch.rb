# An operator write `a <op>= v` the constant fold cannot answer types as the
# call `a.<op>(v)` (`compound_result_type` -> `dispatch_operator`): the
# reference dispatches the entry type through `MethodDispatcher`, so the RBS
# answer stands where the fold raises or the entry holds no literal. The
# port admits only the rows the reference cannot fold whatever the operands'
# precision. Every row is oracle-measured at the current pin; `#944` rows.

# --- FIRES on both sides ----------------------------------------------------

# (1) a raising operand: `String#+` on a non-String, `Integer#<<` on nil.
s1 = "abc"
s1 += nil
s1.w

s2 = "abc"
s2 += 2
s2.w

n1 = 1
n1 <<= nil
n1.w

# (2) `String#+` on an untyped operand answers `String`.
s3 = "abc"
s3 += unresolved_call
s3.w

# (3) a mutated (plain `String`) entry has no literal to fold.
s4 = "abc"
s4 << "d"
s4 *= 2
s4.w

s5 = "abc"
s5 << "d"
s5 *= 0
s5.w

r1 = rand
r1 %= 1
r1.w

# (4) the Tuple shape folds: `tuple_concat`, `tuple_star`, set difference.
a1 = [1]
a1 += [2]
a1.w

a2 = [1]
a2 *= 2
a2.w

a3 = [1]
a3 -= [1]
a3.w

([1] + [2, "x"]).w

# (5) an undecided arm joins the entry with the dispatched answer.
s6 = "abc"
if unresolved_call
  s6 += nil
end
s6.w

# --- SILENT on both sides ---------------------------------------------------

# (6) a union entry takes no non-nil retry (the plain `(x + 1)` would).
u1 = nil
u1 = 1 if unresolved_call
u1 += 1
u1.w
