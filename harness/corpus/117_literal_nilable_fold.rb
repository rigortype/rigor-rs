# `String#getbyte` / `#rindex` / `#byteindex` / `#byterindex` and the scalar
# `#<=>`s fold on literals, extending fixture 113's String-lookup fold to the
# rest of issue #164's nilable core lookups.
#
# Every method's RBS return is `Integer?`. Before this change the tier-3 slot
# answered the bare flat `Integer`, so a literal call that folds to `nil` was
# witnessed on `Integer` — `call.undefined-method` where the oracle is silent —
# and a hit reported `for Integer` where the oracle names the constant.
#
# The fold executes the lookup on the pinned scalars only where it is
# byte-exact with CRuby (the reference folds by running the real method behind
# a purity allowlist). A pinned argument list Ruby raises on — a TypeError
# argument, a wrong arity, an out-of-range Float index — declines to
# `Dynamic`: the reference's fold rescues into the `C?` union, on which no
# negative rule fires (section 4). Non-pinnable literals ride the same union
# or a constant the port cannot carry (Range/Regexp/interpolation, section 5).
#
# Every firing line and every silent row is oracle-measured at the `e59b7b89`
# pin, one fresh temp cwd per case, `--no-cache`, both reference libs pinned
# onto `-I` (UPSTREAM.md hazard 1).

# --- (1) STAYS SILENT: the literal lookup folds to `nil` ----------------------

# The issue's head rows: each folds to `nil`, and `nil.to_a` exists — no
# diagnostic on either engine. A bare `Integer` answer fired here before.
def m1 = "abc".getbyte(9).to_a
def m2 = "abc".getbyte(-9).to_a
def m3 = "abc".rindex("z").to_a
def m4 = "abc".byteindex("z").to_a
def m5 = "abc".byterindex("z").to_a
def m6 = (1.0 <=> "x").to_a

# The same fold, more argument shapes: negative/Float indices, bounded and
# absent searches, `<=>` on every non-comparable arm.
def m7 = "abc".getbyte(9.9).to_a
def m8 = "abc".getbyte(-4).to_a
def m9 = "abc".rindex("b", 0).to_a
def m10 = "abc".rindex("ca", 2).to_a
def m11 = "abc".byteindex("b", 2).to_a
def m12 = "abc".byterindex("b", 0).to_a
def m13 = "abc".index("", 4).to_a
def m14 = (1 <=> "x").to_a
def m15 = (1.0 <=> {}).to_a
def m16 = ("a" <=> 1).to_a
def m17 = (:a <=> "a").to_a
def m18 = "abc".rindex("z", 1).to_a

# --- (2) FIRES `for nil` — the folded nil names `nil`, not `Integer` ----------

def n1 = "abc".getbyte(9).lenght
def n2 = "abc".rindex("z").lenght
def n3 = (1.0 <=> "x").lenght
def n4 = (1 <=> "x").lenght
def n5 = "abc".byterindex("z").lenght

# --- (3) FIRES the folded constant — must-still-fire controls -----------------

# The issue's controls: same rule and location as before, and the message now
# names the reference's constant.
def f1 = "abc".rindex("b").to_a
def f2 = "abc".byteindex("b").to_a
def f3 = "abc".byterindex("b").to_a
def f4 = "abc".getbyte(0).to_a
def f5 = "abc".getbyte(-1).to_a
def f6 = (1.0 <=> 2).to_a
def f7 = (1 <=> 2).to_a
def f8 = ("a" <=> "b").to_a
def f9 = (:a <=> :b).to_a

# Optional start offsets: the bound is on the match's START, negative counts
# from the end.
def f10 = "abc".rindex("b", -1).to_a
def f11 = "abc".rindex("a", 0).to_a
def f12 = "abc".byterindex("b", -2).to_a
def f13 = "abc".index("c", -1).to_a

# A Float index truncates (`getbyte(1.5)` is `getbyte(1)`); byte-domain
# lookups on multibyte receivers answer byte offsets.
def f14 = "abc".getbyte(1.5).to_a
def f15 = "héllo".getbyte(2).to_a
def f16 = "héllo".byteindex("l").to_a
def f17 = "héllo".byterindex("l").to_a
def f18 = "héllo".index("l").to_a
def f19 = "héllo".rindex("l").to_a
def f20 = "héllo"[1].to_a

# --- (4) STAYS SILENT: the pinned argument list raises in Ruby ----------------

# A TypeError argument or offset, a wrong arity, an out-of-long-range Float:
# the reference's fold rescues into the `C?` union and the chained call stays
# silent. The arity/mismatch rules fire on their own rows, unchanged.
def r1 = "abc".getbyte("x").to_a
def r2 = "abc".getbyte(nil).to_a
def r3 = "abc".getbyte(1, 2).to_a
def r4 = "abc".rindex(1).to_a
def r5 = "abc".rindex("b", "x").to_a
def r6 = "abc".rindex("b", 0, 1).to_a
def r7 = "abc".byterindex("b", nil).to_a
def r8 = "abc".index(1).to_a
def r9 = "abc".index("b", "x").to_a
def r10 = "abc".index().to_a
def r11 = "abc".byteslice("a").to_a
def r12 = "abc".getbyte(1e19).to_a

# --- (5) the union, not the fold: literals the pin cannot carry ---------------

# Interpolated and container literals, and pseudo-literals (`__LINE__`), ride
# the reference's `C?` union — the chain stays silent. A Range on `[]` /
# `slice` / `byteslice` still folds to a firing constant, so the flat `String`
# answer stands (set-match on `"bc"` / `"ab"`).
def u1 = "abc"[{}].to_a
def u2(x) = "abc".index("a#{x}").to_a
def u3(x) = "abc".getbyte("a#{x}").to_a
def u4 = "abc".getbyte(__LINE__).to_a
def u5 = "abc"[1..].frobnicate_u5
def u6 = "abc".slice(1..2).frobnicate_u6
def u7 = "abc".byteslice(0..1).frobnicate_u7

# A Regexp argument folds on the reference (`"abc".index(/b/)` is `1`). The
# port keeps the flat answer on the `[]`/`index` rows it already answered —
# the hit set-matches — and declines the lookups it newly learned: their
# `/z/` misses already fired `for Integer` (an FP), so declining the argument
# loses the `/b/` hit's `for Integer` set-match to kill the miss FP.
def u8 = "abc".index(/b/).to_a
def u10 = "abc".rindex(/b/).to_a
def u11 = "abc".rindex(/z/).to_a
def u12 = "abc".getbyte(/x/).to_a
def u13 = "abc"[/b/].frobnicate_u13

# --- (6) OPAQUE: a literal whose unescaped bytes are not valid UTF-8 ----------

# The lowering substitutes U+FFFD for invalid bytes, so a scalar minted from
# such a literal cannot answer a position or content read — the reference
# folds on the REAL bytes and fires the hits, which are registered coverage
# gaps here; every miss stays silent on both engines (#164 review).
def v1 = "\xFFabc".getbyte(5).to_a
def v2 = "\xFFabc".getbyte(-6).to_a
def v3 = "\xFFabc".byteindex("c", 4).to_a
def v4 = "\xE3\x81a"[2].upcase
def v5 = "\xFFa".byterindex("a", 1).succ
def v6 = "\xFFabc".getbyte(0).lenght
def v7 = "\xFFabc".byteindex("a").lenght
def v8 = "\xE3\x81a".index("a").lenght
def v9 = "\xE3\x81a"[2].lenght
def v10 = "\xFFabc"[0].lenght
def v11 = ("\xFFa" <=> "\xFEa").lenght

# --- (7) `byteindex` / `byterindex`: the offset must land on a char boundary --

# Inside a multibyte character Ruby raises `IndexError: offset N does not
# land on character boundary`; the reference rescues into the `C?` union —
# silent — where the port once minted `for Integer` or a wrong constant.
def w1 = "héllo".byteindex("h", 2).succ
def w2 = "héllo".byteindex("l", 2).to_a
def w3 = "héllo".byterindex("h", 2).to_a
def w4 = "héllo".byteindex("", 2).to_a
def w5 = "héllo".byterindex("", 2).to_a
def w6 = "héllo".byteindex("l", -4).to_a
def w7 = "\u{1F600}x".byteindex("x", 1).to_a

# The controls that still answer: boundary offsets fold, a past-end
# `byterindex` clamps to the end, and a past-end `byteindex` is `nil` — none
# of those raise.
def w8 = "héllo".byteindex("l", 3).to_a
def w9 = "héllo".byterindex("o", 6).to_a
def w10 = "héllo".byterindex("o", 100).to_a
def w11 = "héllo".byterindex("h", -6).to_a
def w12 = "héllo".byteindex("l", 6).to_a
def w13 = "héllo".byteindex("l", 7).to_a
