# `call.raise-non-exception` MUST stay silent across its whole zero-FP envelope.
# Every line here is silent in BOTH the oracle and rigor-rs.

# Legal operands: Exception classes, Exception instances, and Strings.
raise StandardError
raise RuntimeError
raise KeyError
raise ArgumentError, "with a message"
raise StandardError.new("built instance")
raise "a plain string message"
raise "an interpolated #{1 + 1} message"

# Bare `raise` re-raises `$!`; an explicit receiver is a user method, not Kernel.
raise
some_object.raise(42)

# A splat / bare keyword-hash first argument bails (the reference's
# `first_positional_raise_operand`); `raise({a: 1})` with braces would fire, but
# the bare keyword form does not.
raise(*collected_errors)
raise(code: 42)

# Unresolved / dynamic operands — never a concrete illegal verdict.
raise NotAConstantAnywhere
raise self.class
raise Unqualified::Nested

# A project-discovered class bails on BOTH paths, even when its written
# superclass is StandardError (a project `sig/` could omit the superclass, so
# source-declared classes stay silent).
class CustomError < StandardError
end
raise CustomError
raise CustomError.new

# Redefinition of `raise` / `fail` where the call could resolve.
module Redefiner
  class Toolbox
    def self.raise(arg)
    end

    def detonate
      raise 42
    end
  end
end
