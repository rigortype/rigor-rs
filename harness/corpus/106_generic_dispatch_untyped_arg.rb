# An UNTYPED argument must not pin the GENERIC RBS dispatch's answer either.
#
# Issue #118, the residue the `v0.3.4 -> v0.3.8` re-pin left open. Upstream #521
# / PR #537 (`3d5dddbb`) selects EVERY overload matching the call's arity and
# block shape and JOINS their returns: one candidate answers its own return,
# identical returns collapse to that return, and distinct returns answer
# `Dynamic[union]`, on which no negative rule fires. The re-pin ported that for
# the Kernel folds only (fixture 99); the generic receiver dispatch still handed
# the rules a bare `Nominal[C]` read off a per-method flat slot.
#
# The flat slot diverges from the reference's join in exactly two measured ways,
# and BOTH are here:
#
# 1. A NILABLE return. `String#[]`'s four overloads all return `String?`; the
#    reference answers `String | nil` and no negative rule fires, while the flat
#    slot drops the nil bit and hands over a bare `String`. This is the issue's
#    own row (1) and its `slice` / `byteslice` / `index` / `rindex` / `getbyte` /
#    `byteindex` / `assoc` / `rassoc` / `Float#<=>` twins.
# 2. Returns that agree only after ERASURE. The slot compares the head class
#    NAME, so `Array#product`'s `Array[[E, X]]` and `Array[Array[E | U]]`
#    "agree" on `Array`; the reference joins them to `Dynamic[union]`. That is
#    #521's own `[true] * n` class of defect — rows (2).
#
# The gate is the same reference-untyped ALLOW-LIST fixture 99 documents
# (`Typer::arg_is_reference_untyped`), for the same reason: rigor-rs cannot ask
# "is this argument exactly `Dynamic[Top]`" from its TYPE, because every def-body
# local read answers `Dynamic[top]`. Untypedness is also what makes the
# divergence OBSERVABLE — with a LITERAL argument the reference constant-folds
# `"abc"[0]` to `"a"` and fires, and the flat slot's bare `String` matches that
# row. Section (3) is that whole control set, and every one of its lines fires on
# both engines before and after this change.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the join is a nilable union, not a bare nominal -----------

# (1) `String#[]` — issue #118's row. All four overloads return `String?`.
def g1(u) = "abc"[u].frobnicate_g1

# ... and its family, each measured silent on the reference and firing here
# before this change.
def g2(u) = "abc".slice(u).frobnicate_g2
def g3(u) = "abc".byteslice(u).frobnicate_g3
def g4(u) = "abc".index(u).frobnicate_g4
def g5(u) = "abc".rindex(u).frobnicate_g5
def g6(u) = "abc".getbyte(u).frobnicate_g6
def g7(u) = "abc".byteindex(u).frobnicate_g7
def g8(u) = [1, 2].assoc(u).frobnicate_g8
def g9(u) = [1, 2].rassoc(u).frobnicate_g9
def g10(u) = (1.5 <=> u).frobnicate_g10

# --- STAYS SILENT: two candidates whose returns differ under the erased head --

# (2) `Array#product` at arity 1 keeps `[X] (array[X]) -> Array[[E, X]]` and
# `[U] (*array[U]) -> Array[Array[E | U]]`; `Array#zip` keeps the analogous
# pair; `String#scan`'s two block-free arms return `Array[String |
# Array[String?]]` and `Array[String]`. All three erase to `Array`.
def g11(u) = [1, 2].product(u).frobnicate_g11
def g12(u) = [1, 2].zip(u).frobnicate_g12
def g13(u) = "abc".scan(u).frobnicate_g13

# --- FIRES: a LITERAL argument, which the reference constant-folds ------------

# (3) The reference folds `"abc"[0]` to `"a"` and witnesses on the literal; the
# flat slot's bare `String` lands on the same row. Withholding on these would be
# a coverage loss, which is why the gate asks for a reference-untyped argument
# and not merely for a nilable return.
def g14 = "abc"[0].frobnicate_g14
def g15 = "abc"[1..].frobnicate_g15
def g16 = "abc".byteslice(1).frobnicate_g16
def g17 = "abc".index("b").frobnicate_g17
def g18 = "abc".slice(1).frobnicate_g18

# (4) The same rows where the fold's VALUE is nil: the reference answers `nil`
# and still fires (on `nil` rather than on `String` — the rule and position
# match, the receiver name does not).
def g19 = "abc"[99].frobnicate_g19
def g20 = "abc"[9..].frobnicate_g20
def g21 = "abc".index("z").frobnicate_g21

# --- FIRES: an untyped argument the candidates all answer the same way --------

# (5) One matching overload, or several declaring the SAME return: the join IS
# that return and both engines witness on it. These are the must-still-fire
# controls a blanket "nilable declines" or "any disagreement declines" rule
# would swallow.
def g22(u) = "abc".center(u).frobnicate_g22
def g23(u) = "abc".ljust(u).frobnicate_g23
def g24(u) = ("abc" * u).frobnicate_g24
def g25(u) = ("a%sb" % u).frobnicate_g25
def g26(u) = "abc".delete_prefix(u).frobnicate_g26
def g27(u) = "abc".tr(u, "b").frobnicate_g27
def g28(u) = "abc".sub("a", u).frobnicate_g28
def g29(u) = "abc".split(u).frobnicate_g29
def g30(u) = [1, 2].take(u).frobnicate_g30
def g31(u) = [1, 2].join(u).frobnicate_g31
def g32(u) = { a: 1 }.merge(u).frobnicate_g32
def g33(u) = 1.gcd(u).frobnicate_g33

# A class-GUARDED parameter (`return unless u.is_a?(Integer)` then `"abc"[u]`)
# is deliberately NOT here: the allow-list refuses it, the reference is silent
# on it, and it stays a pre-existing false positive this slice does not close —
# see `docs/notes/20260909-generic-dispatch-untyped-arg.md` residue 1. It is out
# of the corpus because a fixture may not carry an unregistered extra.
