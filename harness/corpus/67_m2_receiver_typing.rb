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

# --- 4. singleton RBS returns + declaration-driven instance witnessing ------
# `module_function`-style class methods with a unanimous declared return type
# their instance (`Time.now -> Time`, `Date.today -> Date`, the late-bound
# `-> instance`), and an RBS-known TOPLEVEL class's instance witnesses like a
# core class. A singleton alias resolves through its target
# (`alias self.pwd self.getwd` -> String).

t = Time.now
t.frobnicate

d = Date.today
d.end_of_month

w = Dir.pwd
w.frobnicate

# Divergent-overload singleton returns decline by construction
# (`Regexp.last_match`: `MatchData?` vs `String?`) - handled by the P2
# nil-source arm instead, so no UM here on the direct chain.
m = Regexp.last_match(1).frobnicate

# The reference's constant-constructor lifts stay silent: an all-pinned
# `Set.new` / `Date.new(2020)` lifts to a pinned value there, so rigor-rs
# declines the mint (Dynamic).
s = Set.new
s.frobnicate
dd = Date.new(2020)
dd.frobnicate
