# M2-GO receiver-typing batch (compat plan Phase 0 follow-up): three
# mechanisms the reference types and rigor-rs was silent on, each measured on
# gitlab-foss lib (179 -> 155 UM gaps). Byte-for-byte on (rule, line, column).

# --- 1. `CONST = <literal>.freeze` unwraps to the literal (C5 harvest) ------
# RuboCop's Style/MutableConstant autocorrect spelling; `freeze` is identity.

module MetricGenerator
  ALLOWED_OPERATIONS = %w[count sum average].freeze
  LIMITS = { low: 1, high: 10 }.freeze

  def self.validate(operation)
    # AS `exclude?` is absent on core Array -> undefined-method on the Tuple.
    raise ArgumentError if ALLOWED_OPERATIONS.exclude?(operation)
  end

  def self.limits
    # AS `deep_merge!` is absent on core Hash -> undefined-method on the shape.
    LIMITS.deep_merge!({ mid: 5 })
  end
end

# Nested freeze works at any depth.
DOUBLE = [%w[a b].freeze, %w[c d].freeze].freeze
DOUBLE.frobnicate

# A frozen non-literal RHS still declines the harvest (silent: the receiver
# types Dynamic, so nothing is witnessed on it).
DYNAMIC = compute_something.freeze
DYNAMIC.frobnicate

# --- 2. `Kernel#Array` types by argument (nominal Array when undecidable) ---

def coerce(config)
  # AS `presence` is absent on core Array -> undefined-method on Array.
  Array(config).presence
end

# Pinned shapes still fold precisely: identity / nil-collapse / scalar wrap.
a = Array([1, 2])
a.frobnicate
b = Array(nil)
b.frobnicate

# --- 3. `rand` returns: `(int) -> Integer`, `() -> Float` -------------------

def jitter
  # AS `hours` is absent on core Integer -> undefined-method on Integer.
  rand(5).hours
end

def ratio
  rand.frobnicate
end

def unknown_bound(n)
  # ANY non-Range 1-arg call resolves the `(int) -> Integer` overload, matching
  # the reference's measured pick - fires on Integer.
  rand(n).frobnicate
end

def range_bound
  # A Range argument declines (the Range overload returns the element type).
  rand(1..5).frobnicate
end
