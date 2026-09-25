# An UNTYPED argument must not pin one overload.
#
# Upstream #521 / PR #537 (`3d5dddbb`), shipped between `v0.3.4` and `v0.3.8`:
# with a `Dynamic[Top]` argument both the alias pass and the gradual pass used to
# answer the FIRST overload in declaration order — an untyped argument
# "maybe"-accepts every arm, so POSITION decided. The alias pass now declines on
# an untyped argument, the gradual pass returns EVERY arity-and-block-compatible
# overload, and the dispatch JOINS their returns: one candidate keeps its return,
# identical returns collapse to that return, and otherwise the answer is
# `Dynamic[union]`, on which no negative rule fires.
#
# So the conversion's own overload SET decides. `Float`, `Integer` (1- and
# 2-arg), `Array` and `rand` have arms whose returns differ, and go silent;
# `String` and `format`/`sprintf` have one matching arm each, and keep firing.
# Three of the four `v0.3.4 -> v0.3.8` re-pin false positives were this family
# (fixture 60 line 59, fixture 67 lines 36 and 59).
#
# rigor-rs cannot ask upstream's question ("is this argument exactly
# `Dynamic[Top]`") from the argument's TYPE: a use site inside a method body
# reads an EMPTY local env (`ScopedEnv::at` — a `def` body is an independent
# scope), so every def-body local read answers `Dynamic[top]` whether or not the
# reference knows its type. It asks a SYNTACTIC reach analysis instead
# (`Typer::arg_reach`): which values can reach the argument's root, and is one of
# them untyped? The root must not be class-guarded; a parameter's initial value
# reaches unless a DEFINITE rebind on the read's statement path cuts it off. The
# rows of (10) and (11) below are the controls that analysis exists for — a bare
# `Dynamic[top]` test silenced them, and every one of them fires on the
# reference.
#
# #1021 (upstream `5496acd6`, the `v0.3.8 -> e59b7b89` re-pin) widened
# upstream's test from `untyped_arg?` to `imprecise_arg?`: a UNION with an
# untyped member (`Dynamic[top] | "x"`, what `s = "x" if s.nil?` leaves) skips
# the strict pass too, the gradual pass keeps every overload that accepts all
# the members, and the four conversions join to `Dynamic[union]` again. Rows
# a7/a20/q3 moved from FIRE to SILENT with it — section (6b). `rand` alone still
# pins through such a union: only its `(int)` overload accepts a String/nil/...
# member, so it fires unless a member is a Range (section (13)).
#
# Every firing line and every silent control is oracle-measured — at the
# `v0.3.8` pin (`ffb456b0`), and every row re-measured at `e59b7b89` — one fresh
# temp cwd per case, `--no-cache`, both reference libs pinned onto `-I`
# (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the overloads disagree and the argument cannot choose ------

# (1) the four conversions whose Kernel overloads have differing returns, over a
# bare parameter.
def a1(u) = Float(u).frobnicate_a1
def a2(u) = Integer(u).frobnicate_a2
def a4(u) = Array(u).frobnicate_a4
def a5(u) = rand(u).frobnicate_a5

# (2) `Integer`'s 2-arg (base) spelling declines on the same rule.
def a17(u) = Integer(u, 16).frobnicate_a17

# (3) the explicit `Kernel.` receiver spelling routes to the same fold, so it
# must decline there too — the reason the fold ANSWERS `Dynamic[top]` instead of
# returning "no answer" and falling through to the singleton-RBS tier.
def a19(u) = Kernel.Float(u).frobnicate_a19

# (4) an arbitrary call chain over an untyped root is untyped as well — the
# shape fixture 60 line 59 is built from.
def a18(u)
  @dur_a18 = Float(u[:k])
rescue ArgumentError
  @dur_a18 = 0
end

# (5) a TRUTHINESS guard does not type an untyped local — `Dynamic` minus nil is
# still `Dynamic` — so the decline survives it, and `nil?` is deliberately absent
# from the allow-list's class-guard exclusion for exactly that reason.
def a34(u)
  return unless u

  Float(u).frobnicate_a34
end

def a36(u)
  return if u.nil?

  Float(u).frobnicate_a36
end

# (6) a local REBOUND from another untyped value stays untyped.
def q5(s)
  t = s.to_s
  Float(t).frobnicate_q5
end

# (6b) #1021: a rebind that leaves the parameter REACHABLE makes the
# reference's carrier a union with an untyped member (`Dynamic[top] | "x"`),
# which is imprecise and declines exactly like the bare carrier. A conditional
# rebind in every spelling — modifier `if`/`unless`, `||=`, `&&=`, `+=`,
# `s || "x"`, a ternary or `case` arm that keeps `s`, a loop, a block, a
# `begin`/`rescue` body — and a read BEFORE the rebind.
def a7(s)
  s = "x" if s.nil?
  Float(s).frobnicate_a7
end

def a20(s)
  s = "x" if s.nil?
  Array(s).frobnicate_a20
end

def q3(s)
  s ||= "x"
  Float(s).frobnicate_q3
end

def u1(s)
  s = "x" if s.nil?
  Integer(s).frobnicate_u1
end

def u2(s)
  s = "x" if s.nil?
  Integer(s, 16).frobnicate_u2
end

def u3(s)
  s = s || "x"
  Float(s).frobnicate_u3
end

def u4(s)
  s = "x" unless s
  Float(s).frobnicate_u4
end

def u5(s, c)
  s = case c
      when 1 then "x"
      else s
      end
  Float(s).frobnicate_u5
end

def u6(s, c)
  s = "x" if c
  Float(s).frobnicate_u6
end

def u7(s, c)
  s = s.to_s if c
  Float(s).frobnicate_u7
end

def u8(s, c)
  while c
    s = "x"
  end
  Float(s).frobnicate_u8
end

def u9(s)
  [1].each { s = "x" }
  Float(s).frobnicate_u9
end

def u10(s)
  begin
    s = "x"
  rescue StandardError
    nil
  end
  Float(s).frobnicate_u10
end

def u11(s)
  s &&= "x"
  Float(s).frobnicate_u11
end

def u12(s)
  s += "x"
  Float(s).frobnicate_u12
end

def u13(s)
  Float(s).frobnicate_u13
  s = "x"
end

def u14(s, c)
  s = c ? "x" : s
  Float(s).frobnicate_u14
end

def u15(s, c)
  t = "x"
  t = s if c
  Float(t).frobnicate_u15
end

def u16(s)
  s = "x" if s.nil?
  s = s.strip
  Float(s).frobnicate_u16
end

def u17(s)
  s = "x" if s.nil?
  t = s
  Float(t).frobnicate_u17
end

def u18(h)
  h.each do |_k, v|
    v = "x" if v.nil?
    Float(v).frobnicate_u18
  end
end

# (6c) every parameter kind is the untyped carrier — a default, a keyword.
def u19(s = "x") = Float(s).frobnicate_u19
def u20(s: "x") = Float(s).frobnicate_u20

# --- MUST STILL FIRE: one candidate, or an argument that discriminates --------

# (7) `String` and `format`/`sprintf` have a single matching overload, so the
# join IS that return and the untyped argument changes nothing.
def a3(u) = String(u).frobnicate_a3
def a9(u) = format("%d", u).frobnicate_a9
def a33(u) = sprintf("%d", u).frobnicate_a33

# (8) a value-pinned literal argument still folds, and still pins.
def a6 = Float("1.5").frobnicate_a6
def a8 = rand(5).frobnicate_a8
def a21 = Array([1, 2]).frobnicate_a21
def a22 = Array(5).frobnicate_a22
def a24 = Integer("1f", 16).frobnicate_a24

# (9) `rand` with NO argument has one candidate and answers Float.
def a23 = rand.frobnicate_a23

# (10) a local whose untyped value CANNOT reach the read is typed on the
# reference, and every conversion keeps its pin: an unconditional rebind (of a
# fresh local or of a parameter), an `if`/`else` or `case`/`else` rebinding on
# every arm, an arm that returns instead, a rebind nested in the same block or
# branch as the read, a later unconditional rebind after a conditional one, and
# a plain local that starts as `nil` (`t = nil; t ||= "x"`, or a conditional
# write whose other path leaves `nil`). These are the rows a naive
# `Dynamic[top]` test — or a naive "any conditional write declines" — silences.
def q4
  s = "x"
  Float(s).frobnicate_q4
end

def v1(s)
  s = "x"
  Float(s).frobnicate_v1
end

def v2(s, c)
  if c
    s = "x"
  else
    s = "y"
  end
  Float(s).frobnicate_v2
end

def v3(s, c)
  case c
  when 1 then s = "a"
  when 2 then s = "b"
  else s = "c"
  end
  Float(s).frobnicate_v3
end

def v4(s, c)
  if c
    s = "x"
  else
    return
  end
  Float(s).frobnicate_v4
end

def v5(s)
  [1].each do
    s = "x"
    Float(s).frobnicate_v5
  end
end

def v6(s, c)
  if c
    s = "x"
    Float(s).frobnicate_v6
  end
end

def v7(s)
  s = "x" if s.nil?
  s = "y"
  Float(s).frobnicate_v7
end

def v8
  t = nil
  t ||= "x"
  Float(t).frobnicate_v8
end

def v9(c)
  t = "a" if c
  t ||= "x"
  Float(t).frobnicate_v9
end

def v10(c)
  t = "x" if c
  Float(t).frobnicate_v10
end

def v11(s)
  s = "x"
  s = s.upcase
  Float(s).frobnicate_v11
end

def v12(items)
  items.map do |v|
    t = "a"
    Float(t).frobnicate_v12(v)
  end
end

# (10b) a union with NO untyped member discriminates as before.
def v13(flag)
  s = flag ? "x" : 1
  Float(s).frobnicate_v13
end

# (11) a parameter the body CLASS-GUARDS is narrowed to a Nominal and keeps its
# pin too — the second half of the allow-list.
def q6(s)
  return unless s.is_a?(String)

  Float(s).frobnicate_q6
end

def a25(s)
  return unless s.is_a?(String)

  Integer(s, 16).frobnicate_a25
end

def a31(s)
  return unless s.is_a?(Integer)

  rand(s).frobnicate_a31
end

# (12) a non-Kernel receiver whose overloads AGREE keeps its answer under an
# untyped argument — the join is that one return.
def a13(u) = "abc".center(u).frobnicate_a13

# (11b) a class guard AFTER a `Dynamic[top] | "x"` rebind still narrows the
# union to a Nominal, and `String` (one candidate) ignores the union entirely.
def w1(s)
  s = "x" if s.nil?
  return unless s.is_a?(String)

  Float(s).frobnicate_w1
end

def w2(s)
  s = "x" if s.nil?
  String(s).frobnicate_w2
end

# (13) `rand` over a union with an untyped member: only `(int)` accepts a
# String, nil, Integer, ... member leniently, so the join is `Integer` and the
# reference FIRES; a Range member keeps a Range overload alive and declines, as
# does the bare carrier (row a5).
def w3(s)
  s = "x" if s.nil?
  rand(s).frobnicate_w3
end

def w4(s)
  s = 1 if s.nil?
  rand(s).frobnicate_w4
end

def w5(c)
  t = c if c
  rand(t).frobnicate_w5
end

def w6(s)
  s = (1..2) if s.nil?
  rand(s).frobnicate_w6
end
