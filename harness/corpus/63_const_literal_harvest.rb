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

# --- STAYS SILENT ----------------------------------------------------------

# A constant defined in a MODULE is not lexically visible in an unrelated class
# that merely includes it — the reference resolves lexically, so folding it
# there would be wrong (and would manufacture an ActiveSupport `Integer#days`
# style FP). `DAYS` is not in `Consumer`'s lexical nesting ⇒ Dynamic ⇒ silent.
module Expirable
  DAYS = 7
end

class Consumer
  include Expirable

  def go
    DAYS.frobdays # NOT folded (DAYS lives in Expirable, not visible here).
  end
end

# A constant assigned MORE THAN ONCE is ambiguous ⇒ declined ⇒ silent.
class MultiAssign
  M = 1
  M = 2
  def go
    M.frobmulti
  end
end
