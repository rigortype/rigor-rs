# `call.unresolved-toplevel` (ref ADR-34) fires on an implicit-self call at
# TOPLEVEL scope (outside any class/module body) whose name resolves against
# neither the Object/Kernel instance surface nor a same-file toplevel `def`.
# Byte-for-byte against the oracle on (rule, line, column = the method token).

# Kernel/Object methods resolve (all declared `def self?.x` in core RBS, so
# recorded as instance methods on Kernel, which Object includes) -> NO fire.
puts "resolved"
require "set"
require_relative "x"
loop { break }

# Genuinely-undefined toplevel calls -> FIRE (the reference routes them to
# `pre_eval:` in the message).
undefined_toplevel_thing
another_missing_call(1, 2)

# A same-file toplevel `def` resolves a toplevel call to it -> NO fire on `helper`.
# But an unresolved implicit-self call INSIDE a toplevel def body IS toplevel
# (scope.toplevel? = outside any class/module) -> FIRE on `still_missing`.
def helper
  still_missing
end
helper

# An implicit-self call inside a `class`/`module` body is NOT toplevel
# (ADR-24 leniency) -> NO fire, even when unresolved.
class Widget
  some_class_macro
  def run
    unresolved_in_method
  end
end
