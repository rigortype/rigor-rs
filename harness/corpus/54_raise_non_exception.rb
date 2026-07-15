# `call.raise-non-exception` fires on an implicit-self `raise` / `fail` whose
# first positional operand's inferred type is provably NOT a legal raise operand
# (not an Exception class / instance, not a String, no `#exception` duck).
# Byte-for-byte against the oracle on (rule, line, column = the raise/fail token).

# Scalar operands — Integer / Symbol / nil / Float. `fail` names itself.
raise 42
raise :sym
raise nil
fail 3.14

# A bare class object disjoint from Exception fires with `singleton(X)`. The
# SINGLETON path applies NO module / generic-carrier exclusion, so a module
# constant and the generic carriers all fire (unlike the instance path).
raise Array
raise Struct
raise Integer
raise Comparable
raise Class
raise Object
raise Module
raise BasicObject

# An instance of a core class disjoint from Exception → the bare class name.
raise Time.new

# A positional (braced) hash literal → the value-pinned `{ a: 1 }`.
raise({ a: 1 })

# NOT toplevel-restricted — fires inside method and class bodies too.
def detonate
  raise 7
end

class Widget
  def go
    raise 99
  end
end

# Only the first positional argument is checked (message + backtrace ignored).
raise 42, "message", caller
