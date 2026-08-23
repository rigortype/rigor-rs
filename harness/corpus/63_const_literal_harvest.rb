# C5 (const-literal harvest): a constant assigned a SINGLE fully-literal value,
# LEXICALLY visible at the use site, types to that literal — so a typo on it is
# witnessed on the value-pinned receiver exactly as an inline literal would be.
# The gate is lexical (Ruby constant lookup) AND single-assignment AND
# not-a-class, matching the reference. Byte-for-byte on (rule, line, column).

class K
  # Same-namespace fully-literal constants (visible where used).
  A = [:a, :b]
  H = { t: 10 }
  S = "hello"
  N = 42

  # --- FIRES: witnessed on the value-pinned receiver ------------------------

  def arr
    A.frobarr    # [:a, :b].frobarr
  end

  def hsh
    H.frobhash   # { t: 10 }.frobhash
  end

  def str
    S.frobstr    # "hello".frobstr
  end

  def int
    N.frobint    # 42.frobint
  end
end

# --- A COVERAGE GAP as of the `v0.3.4` pin ----------------------------------

# A constant defined in a MODULE and reached from a class that `include`s it.
# Through the `v0.3.2` pin the reference resolved constants LEXICALLY only, so
# `DAYS` — not in `Consumer`'s lexical nesting — stayed `Dynamic` and both sides
# were silent; folding it would have manufactured an ActiveSupport
# `Integer#days` style FP.
#
# Upstream `1eda3dcf` / #356 (in `v0.3.4`) added the middle step of Ruby's
# three-step lookup — each entry of `Module.nesting`, THEN the ancestors of the
# innermost cresting scope, then the top level — so the reference now folds
# `DAYS` to `7` and witnesses the typo. rigor-rs still resolves lexically: a
# COVERAGE GAP, the sound-subset direction, never an FP.
module Expirable
  DAYS = 7
end

class Consumer
  include Expirable

  def go
    DAYS.frobdays # reference folds to 7 via the ancestor chain; rigor-rs declines.
  end
end

# --- STAYS SILENT ----------------------------------------------------------

# A constant assigned MORE THAN ONCE is ambiguous ⇒ declined ⇒ silent.
class MultiAssign
  M = 1
  M = 2
  def go
    M.frobmulti
  end
end
