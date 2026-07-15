# `flow.shadowed-rescue-clause` fires on a later `rescue` clause of a
# begin/def rescue chain whose EVERY named exception class is already caught by
# an earlier clause of the SAME chain (the later clause is dead). Anchor = the
# later `rescue` keyword; byte-for-byte against the oracle on (rule, line, col).

# A wide earlier clause shadows a narrow subclass later.
begin
  y = 1
rescue StandardError
  y = 2
rescue ArgumentError
  y = 3
end

# A bare `rescue` counts as `StandardError`, shadowing a later ArgumentError;
# its rendered source is just `rescue`.
begin
  y = 1
rescue => e
  y = 2
rescue ArgumentError
  y = 3
end

# An exact duplicate class is shadowed too.
begin
  y = 1
rescue ArgumentError
  y = 2
rescue ArgumentError
  y = 3
end

# A multi-class arm is dead when EVERY name it lists is covered by one earlier
# clause; the whole `rescue A, B` is rendered.
begin
  y = 1
rescue StandardError
  y = 2
rescue ArgumentError, TypeError
  y = 3
end

# Multiple covering earlier clauses join with " and " and pluralize to `clauses`.
begin
  y = 1
rescue ArgumentError
  y = 2
rescue TypeError
  y = 3
rescue ArgumentError, TypeError
  y = 4
end

# Three clauses: the second AND third are each shadowed by the first — one
# diagnostic per dead clause.
begin
  y = 1
rescue StandardError
  y = 2
rescue ArgumentError
  y = 3
rescue TypeError
  y = 4
end

# Absolute `::` paths resolve (bypassing the lexical ladder) and render raw.
begin
  y = 1
rescue ::StandardError
  y = 2
rescue ::ArgumentError
  y = 3
end

# `Exception` shadows the narrower `StandardError` written after it.
begin
  y = 1
rescue Exception
  y = 2
rescue StandardError
  y = 3
end

# A def-level rescue chain (Prism wraps the body in a begin) fires the same.
# Bare implicit-self calls keep the def body quiet (no dead-assignment /
# unresolved-toplevel noise) so the fixture stays focused on this rule.
def handle
  attempt
rescue StandardError
  recover
rescue ArgumentError
  fallback
end

# A project class with a discovered `< StandardError` superclass certifies and
# is shadowed by an earlier StandardError (direct chain link).
class CustomError < StandardError
end

begin
  y = 1
rescue StandardError
  y = 2
rescue CustomError
  y = 3
end

# The project superclass chain is walked transitively (Leaf < Mid < StandardError).
class Mid < StandardError
end

class Leaf < Mid
end

begin
  y = 1
rescue StandardError
  y = 2
rescue Leaf
  y = 3
end
