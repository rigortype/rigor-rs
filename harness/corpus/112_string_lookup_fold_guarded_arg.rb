# `String#[]` / `#slice` / `#byteslice` / `#index` fold on literals, and a
# class-GUARDED argument no longer pins the flat nilable slot (issue #121).
#
# Fixture 106 closed the UNTYPED-argument form of `"abc"[u].typo` and left the
# GUARDED one standing: `return unless u.is_a?(Integer)` makes `u` precise on
# the reference, so the untyped allow-list refuses it (rows a25/a31 of the
# Kernel-fold arc depend on that refusal), yet the reference still answers
# `String?` — narrowing the ARGUMENT does not make the RETURN non-nil — and no
# negative rule fires on that union.
#
# The issue's ordering is followed. Step 1 teaches the Rust fold core the four
# lookups, so a literal receiver with pinned arguments folds to the value Ruby
# produces, `nil` included (sections 1-3). Step 2 lets tier 3 give up the
# flat nilable slot for a guarded PARAMETER — a bare local that only the untyped
# carrier reaches before the guard narrows it, which is never a value the
# reference can fold (section 4). Section 5 holds the must-still-fire controls
# around that gate.
#
# Every firing line and every silent control is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache`, both reference libs
# pinned onto `-I` (UPSTREAM.md hazard 1).

# --- (1) FIRES: the fold's value is a String or an Integer --------------------

# Both engines fired on these before (the port on a bare `String` / `Integer`);
# the fold keeps every row on the same (rule, line, column).
def s1 = "abc"[0].frobnicate_s1
def s2 = "abc"[-1].frobnicate_s2
def s3 = "abc"[1, 2].frobnicate_s3
def s4 = "abc"[3, 1].frobnicate_s4
def s5 = "abc"[-2, 5].frobnicate_s5
def s6 = "abc"["b"].frobnicate_s6
def s7 = "abc".slice(0).frobnicate_s7
def s8 = "abc".slice(1, 1).frobnicate_s8
def s9 = "abc".byteslice(0).frobnicate_s9
def s10 = "abc".byteslice(1, 2).frobnicate_s10
def s11 = "abc".index("b").frobnicate_s11
def s12 = "abc".index("b", 1).frobnicate_s12
def s13 = "abc".index("", 3).frobnicate_s13
def s14 = "abc".index("c", -1).frobnicate_s14

# --- (2) FIRES `call.undefined-method` for nil — NOT possible-nil-receiver ----

# The reference names `nil` as the receiver; routing a folded nil into the
# nil-receiver rule instead would be a rule swap at the same position.
def n1 = "abc"[99].frobnicate_n1
def n2 = "abc"[-4].frobnicate_n2
def n3 = "abc"[4, 1].frobnicate_n3
def n4 = "abc"[1, -1].frobnicate_n4
def n5 = "abc"["z"].frobnicate_n5
def n6 = "abc".byteslice(5).frobnicate_n6
def n7 = "abc".index("z").frobnicate_n7
def n8 = "abc".index("b", 2).frobnicate_n8
def n9 = "abc".index("", 4).frobnicate_n9

# A method String HAS fires too: the folded receiver is nil. This one is new —
# the bare `String` answer knew `upcase`.
def n10 = "abc"[99].upcase

# --- (3) FIRES `flow.always-truthy-condition` on the folded value — new ------

def t1
  if "abc"[99]
    :never
  end
end

def t2
  if "abc".index("z")
    :never
  end
end

def t3
  if "abc"[0]
    :always
  end
end

# --- (3b) DECLINES — the fold cannot pin these, so nothing moves --------------

# A Float index, a Regexp, a Range and a multibyte receiver all fold on the
# reference; the core declines them and the RBS answer still lands on the same
# row.
def d1 = "abc"[1.5].frobnicate_d1
def d2 = "abc"[/b/].frobnicate_d2
def d3 = "abc"[1..].frobnicate_d3
def d4 = "héllo"[1].frobnicate_d4
def d5 = "héllo".index("l").frobnicate_d5

# --- (4) STAYS SILENT: a class-guarded parameter — issue #121 ----------------

def g1(u)
  return unless u.is_a?(Integer)
  "abc"[u].frobnicate_g1
end

def g2(u) = (u.is_a?(Integer) ? "abc".byteslice(u).frobnicate_g2 : nil)
def g3(u) = (u.is_a?(String) ? "abc".index(u).frobnicate_g3 : nil)

def g4(u)
  return unless u.kind_of?(Integer)
  "abc".slice(u).frobnicate_g4
end

def g5(u)
  return unless u.instance_of?(String)
  "abc".index(u).frobnicate_g5
end

# A second, untyped argument beside the guarded one.
def g6(u, v)
  return unless u.is_a?(Integer)
  "abc"[u, v].frobnicate_g6
end

# A pinned second argument does not make the call foldable either.
def g7(u)
  return unless u.is_a?(Integer)
  "abc"[u, 1].frobnicate_g7
end

# The nilable RBS family beyond String.
def g8(u)
  return unless u.is_a?(Integer)
  [1, 2].index(u).frobnicate_g8
end

def g9(u)
  return unless u.is_a?(Float)
  (1.5 <=> u).frobnicate_g9
end

def g10(u)
  return unless u.is_a?(Integer)
  "abc".getbyte(u).frobnicate_g10
end

# A guard that does not dominate the read leaves the parameter untyped there.
def g11(u)
  u.is_a?(Integer) && :checked
  "abc"[u].frobnicate_g11
end

# A method String HAS: no possible-nil-receiver either.
def g12(u)
  return unless u.is_a?(Integer)
  "abc"[u].upcase
end

G13 = ->(u) { u.is_a?(Integer) ? "abc"[u].frobnicate_g13 : nil }

# --- (5) FIRES: the controls around the step-2 gate ---------------------------

# A precise write reaching the guarded read: the reference folds `"abc"[1]`.
def c1(u)
  u = 1
  return unless u.is_a?(Integer)
  "abc"[u].frobnicate_c1
end

# A rebind after the guard kills the parameter: the reference folds `"abc"[2]`.
def c2(u)
  return unless u.is_a?(Integer)
  u = 2
  "abc"[u].frobnicate_c2
end

# A NON-nilable return keeps the flat slot under a guard.
def c3(u)
  return unless u.is_a?(Integer)
  "abc".center(u).frobnicate_c3
end

# The erasure family is not the nilable one: a typed argument narrows it back
# to overloads the reference joins to a concrete Array, and it fires.
def c4(u)
  return unless u.is_a?(Array)
  [1, 2].product(u).frobnicate_c4
end

def c5(u)
  return unless u.is_a?(Integer)
  [1, 2].zip(u).frobnicate_c5
end

def c6(u)
  return unless u.is_a?(String)
  "abc".scan(u).frobnicate_c6
end

# The Kernel-fold rows a25/a31 (fixture 99): a guarded argument is precise, and
# the conversion's single overload fires on it.
def c7(s)
  return unless s.is_a?(String)
  Integer(s, 16).frobnicate_c7
end

def c8(s)
  return unless s.is_a?(Integer)
  rand(s).frobnicate_c8
end
