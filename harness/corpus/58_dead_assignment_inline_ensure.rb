# flow.dead-assignment on a def with an inline `ensure`: the lowering appends
# the ensure statements to the same BeginRescue `body`, so the trailing
# statement must resolve to the PROTECTED-body tail, not the ensure tail.
#
# `kept` is the method's implicit return value (the ensure clause's value is
# discarded) -> must NOT fire, even though `return 1` follows it in the
# lowered body. Regression guard for the pre-existing FP where the ensure
# tail shadowed the protected tail.
def protected_tail_returns
  kept = 1
ensure
  return 1
end

# The flip side: a write in the ensure clause is NOT the method's return
# value. `leaked` is written and never read -> MUST fire (previously missed
# because the ensure tail was wrongly treated as the implicit return).
def ensure_write_is_dead
  :value
ensure
  leaked = 2
end
