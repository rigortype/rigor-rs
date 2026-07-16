# C2 (parameter default-value checking): the reference walks a parameter's
# DEFAULT-VALUE expression (positional and keyword), including nested calls
# within — a typo on a literal/constant receiver there is witnessed exactly as
# in a method body. rigor-rs lowers each default expression into the arena so
# the call rules reach it. Params themselves stay unbound (Dynamic ⇒ silent),
# so only a literal/constant receiver in a default ever fires — the FP-safe
# subset. Byte-for-byte against the oracle on (rule, line, column).

class K
  # --- FIRES ---------------------------------------------------------------

  # Positional default: a bare `Time` types singleton(Time); `.current`
  # (ActiveSupport, absent from core RBS) is witnessed.
  def positional(t = Time.current)
    t
  end

  # Keyword default: same singleton witnessing in a kwarg position.
  def keyword(a: Time.current)
    a
  end

  # A nested call inside a default: the Array literal `[1, 2]` types Tuple
  # (erasing to Array), so a typo'd method is witnessed on `[1, 2]`.
  def nested(x = [1, 2].frobnicate)
    x
  end

  # A String literal receiver in a default.
  def strdefault(y = "abc".frobz)
    y
  end

  # --- STAYS SILENT --------------------------------------------------------

  # A default that reads ANOTHER param (`a`) types Dynamic (params unbound),
  # so `.anything` on it is silent — the FP-safe subset.
  def param_ref(a, b = a.whatever)
    [a, b]
  end

  # A default that is a plain literal has no call to witness.
  def plain(n = 42)
    n
  end
end
