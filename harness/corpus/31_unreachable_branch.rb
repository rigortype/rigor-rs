# `flow.unreachable-branch` fires on an if/unless whose predicate is a SYNTACTIC
# literal that makes one branch dead (when that dead branch node is present).
# Byte-for-byte against the oracle on (rule, line, column = the DEAD branch's
# anchor). The KEYWORD INVERTS which branch is dead, so the `if` and `unless`
# cases below anchor on OPPOSITE branches for the SAME falsey predicate — the
# parity keystone (anchoring on the wrong branch would land on live code).

# `if false…else…` — falsey predicate, THEN branch dead. Anchor: the dead
# then-branch's first statement (always falsey).
if false
  dead_then
else
  live_else
end

# `unless false…else…` — the keyword INVERTS: falsey predicate kills the ELSE
# branch. Anchor: the `else` keyword (always falsey).
unless false
  live_then
else
  dead_else
end

# `if true…else…` — truthy predicate kills the ELSE branch. Anchor: the `else`
# keyword (always truthy).
if true
  live
else
  dead
end
