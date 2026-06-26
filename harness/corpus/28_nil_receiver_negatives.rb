# possible-nil-receiver NEGATIVES: each method guards/narrows the nilable local
# (or the method is sound on the nil arm), so NEITHER tool fires. Parity = both
# silent. The reference narrows via flow analysis; rigor-rs declines via the
# conservative whole-method-body DECLINE scan.

# `.nil?` early-return guard narrows nil away before the use.
def guard_nil_return
  s = String.new
  x = s.byteslice(0, 2)
  return if x.nil?
  x.upcase
end

# Truthy `unless` guard (raise) narrows nil away.
def guard_raise_unless
  s = String.new
  x = s.byteslice(0, 2)
  raise unless x
  x.upcase
end

# `x` in an `if` predicate narrows the then-branch.
def guard_if_predicate
  s = String.new
  x = s.byteslice(0, 2)
  if x then x.upcase end
end

# `x` as a `&&` operand narrows the right operand.
def guard_and_operand
  s = String.new
  x = s.byteslice(0, 2)
  x && x.upcase
end

# Safe-navigation short-circuits on nil ⇒ not a bug.
def safe_nav
  s = String.new
  x = s.byteslice(0, 2)
  x&.upcase
end

# `||=` reassignment removes the nil possibility.
def reassign_or_eq
  s = String.new
  x = s.byteslice(0, 2)
  x ||= "d"
  x.upcase
end

# `to_s` is defined on NilClass ⇒ the call is sound on the nil arm.
def method_on_nilclass
  s = String.new
  x = s.byteslice(0, 2)
  x.to_s
end
