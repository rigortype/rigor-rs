# Adversarial FP guard for flow.dead-assignment: every SKIP case packed into
# named method bodies. This file must yield ZERO diagnostics on BOTH rigor-rs
# and the reference. If any dead-assignment fires here it is a false positive.
# Tail/RHS expressions use literals (no bare top-level calls) so the file is
# genuinely clean on both sides.

# 1. Trailing write = implicit return -> silent.
def trailing
  kept = 1
end

# 2. `_`-prefixed name = intentionally unused -> silent.
def underscore
  _ignored = 1
  42
end

# 3. Op-write reads its target -> the plain write is read -> silent.
def op_write
  total = 0
  total += 1
  99
end

# 4. ||= / &&= also read the target -> silent.
def logical_op_write
  flag = false
  flag ||= true
  flag &&= false
  :done
end

# 5. Read inside a block counts as a read -> silent.
def block_read
  collected = []
  [1, 2, 3].each { |n| collected << n }
  :finish
end

# 6. Read inside string interpolation counts as a read -> silent.
def interpolation_read
  label = "x"
  message = "value=#{label}"
  message
end

# 7. Multi-write (`a, b = ...`) lowers without a plain write -> silent.
def multi_write
  a, b = [1, 2]
  a + b
end

# 8. Nested def isolation: the OUTER `seed` is read AFTER the inner def, so it is
#    read; the inner def's own body has no dead write either.
def outer_scope
  seed = 1
  def inner_scope
    used = 2
    used
  end
  seed
end

# 9. A write that IS later read in the same body -> silent.
def read_after_write
  computed = "hi"
  computed.to_s
  :trailer
end
