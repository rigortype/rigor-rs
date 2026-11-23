# A certainty-folded `if`/`unless` runs ONE arm unconditionally, so the
# mutations inside its live arm are straight-line code: the reference lands
# each one at its own position (`WidenMutatedLocals` sees `Uncond` marks
# through the folded edge) and mints the mutator's nominal, and a
# `local[k].<mutator>` element-read widens THROUGH the literal key
# (`ElementReadWidening.widen_element_read`) instead of flooring the whole
# binding to `Dynamic`.
#
# Every row is oracle-measured at the current pin; `#774` rows.

# --- FIRES on both sides: the folded arm's mutations land -------------------

# (1) a `<<` on the rebound pin mints `String`.
x = "a"
if true
  x = "s"
  x << "c"
end
x.w

# (2) an arm-local pin widens and `s += "e"` answers `non-empty-string`
# (the plain-call `String + non-empty` projection the op-write expands to).
if true
  s = "abc"
  s << "d"
  s += "e"
end
s.w

# (3) `||=` / `&&=` fold against the widened `String`, not against
# `Dynamic`.
s = "abc"
if true
  s << "d"
  s ||= "e"
end
s.w

s = "abc"
if true
  s << "d"
  s &&= "e"
end
s.w

# (4) a widening that declines keeps the binding it saw — `Integer#<<`
# mutates nothing, so `x` keeps `1`.
x = "a"
if true
  x, y = 1, "s"
  x << "b"
end
x.w

# (5) `h[:a] = 1` rebuilds the `{}` seed to `Hash[Dynamic | Symbol,
# Dynamic | Integer]`.
h = {}
if true
  h[:a] = 1
end
h.w

# (6) `h[:a] << 1` widens THROUGH `:a` — the shape survives.
h = { a: [] }
if true
  h[:a] << 1
end
h.w

# (7) the folded-DEAD arm's mutations never reach the continuation.
x = "a"
if false
  x << "b"
end
x.w

# (8) an undecided predicate that still reads the carrier keeps its
# binding — the oracle's join declines back to `{ a: [] }` too.
h = { a: [] }
if h.empty?
  h[:a] << 1
end
h.w

# --- element reads on straight-line code ------------------------------------

# (9) an element whose widening declines keeps the whole binding.
a = [1]
a[0] << 1
a.w

h = { a: 1 }
h[:a] << 1
h.w

# (10) `<<` on a String element loses the pin to `String`.
h = { a: "s" }
h[:a] << "t"
h.w

# (11) a Tuple rebuilds only the selected position.
a = [[1], [2]]
a[0] << 5
a.w

# --- conditional mutations --------------------------------------------------

# (12) a mutation in an UNDECIDED arm may not have run: the oracle
# answers the union `"abc" | String`; the port's flat env cannot express
# the union join and declines (coverage loss, expected missing).
s = "abc"
if c
  s << "d"
end
s.w

# (13) same for a nested undecided arm INSIDE the live folded arm — the
# oracle answers `"a" | String`, the port declines.
x = "a"
if true
  if c
    x << "b"
  end
end
x.w

# (14) a conditional `[]=` store declines on BOTH engines.
h = { a: "s" }
if c
  h[:a] = "b"
end
h.w
