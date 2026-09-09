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
# reference knows its type. It asks a SYNTACTIC allow-list instead
# (`Typer::arg_is_reference_untyped`): the argument's root is a local the
# enclosing `def` never class-guards, and either never rebinds or rebinds only
# from values that are themselves untyped. Rows 4/5/6 and 8/9/10 below are the
# controls that allow-list exists for — a bare `Dynamic[top]` test silenced all
# six, and every one of them fires on the reference.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1).

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

# (10) a parameter the body REBINDS is typed on the reference — its carrier is a
# union, not the untyped carrier — and every conversion keeps its pin. These are
# the rows a naive `Dynamic[top]` test silences.
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

def q4
  s = "x"
  Float(s).frobnicate_q4
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
