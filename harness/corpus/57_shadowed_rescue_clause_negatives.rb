# `flow.shadowed-rescue-clause` MUST stay silent on every row here — each is a
# load-bearing FP-safety case for the reference's ancestry-certainty envelope.
# The oracle emits NOTHING for this file; rigor-rs must match (zero extras).

# Narrow -> wide: a later clause naming a SUPERCLASS of an earlier one is the
# normal correct rescue order.
begin
  y = 1
rescue ArgumentError
  y = 2
rescue StandardError
  y = 3
end

# `StandardError` then the wider `Exception` — silent (later is a superclass).
begin
  y = 1
rescue StandardError
  y = 2
rescue Exception
  y = 3
end

# Disjoint sibling classes never shadow each other.
begin
  y = 1
rescue ArgumentError
  y = 2
rescue TypeError
  y = 3
end

# Partial coverage: only one name of the multi-class arm is covered, so it
# survives.
begin
  y = 1
rescue ArgumentError
  y = 2
rescue ArgumentError, TypeError
  y = 3
end

# An unresolved constant makes its clause opaque (never resolves to a class).
begin
  y = 1
rescue Foo::Bar
  y = 2
rescue Foo::Bar
  y = 3
end

# A MODULE designator never certifies (module `===` is custom).
begin
  y = 1
rescue Kernel
  y = 2
rescue Kernel
  y = 3
end

# A splat designator is opaque (dynamic set of classes).
ERRORS = [ArgumentError].freeze

begin
  y = 1
rescue StandardError
  y = 2
rescue *ERRORS
  y = 3
end

# A dynamic (local-variable) designator is opaque.
klass = ArgumentError

begin
  y = 1
rescue StandardError
  y = 2
rescue klass
  y = 3
end

# A bare `class Foo` (no discovered superclass) is indistinguishable from a
# module in the discovery table, so it stays uncertified.
class PlainError
end

begin
  y = 1
rescue StandardError
  y = 2
rescue PlainError
  y = 3
end

# A nested `begin` chain is NEVER compared against the enclosing chain.
begin
  begin
    y = 1
  rescue ArgumentError
    y = 2
  end
rescue StandardError
  y = 3
end

# A single-clause chain has nothing to shadow.
begin
  y = 1
rescue StandardError
  y = 2
end

# `# rigor:disable` on the dead clause line suppresses the diagnostic.
begin
  y = 1
rescue StandardError
  y = 2
rescue ArgumentError # rigor:disable flow.shadowed-rescue-clause
  y = 3
end
