# encoding: binary
# A magic `encoding:` comment is file-scoped — Ruby honors it on line 1, or
# line 2 after a shebang — so the non-UTF-8 rows of the issue #164 fix round
# live in their own fixture: putting the comment in fixture 117 would flip
# every raw multibyte literal there to byte-per-char semantics and erase the
# UTF-8 fold coverage it was written to pin.
#
# Under `binary` (and `us-ascii` / `ascii-8bit`) Ruby reads a literal's bytes
# under a character width the port's UTF-8-decoded scalar does not share:
# `"\xC3\xA9"` is TWO one-byte characters here, not the single character the
# UTF-8 decoding answers. A char-position or content fold on a non-ASCII
# scalar would mint the wrong constant
# (`"\xC3\xA9a"[1]` is `"\xA9"`, not `"a"`), so the fold declines it like an
# invalid-UTF-8 literal — the lookups go `Opaque` and the scalar `<=>`s
# decline. The reference executes real Ruby and still fires the hits, so the
# rows where the reference reports are coverage gaps; the misses stay silent
# on both engines. Every literal uses `\x` escapes so the file is all-ASCII.
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- (1) STAYS SILENT: the fold declines on the non-ASCII literal -----------

# The review's head FP rows: each fired `for nil` / a wrong constant on the
# port before the encoding gate, where the oracle stays silent — a binary
# `"\xC3\xA9"[1]` is `"\xA9"`, never `nil`.
def b1 = "\xC3\xA9"[1].upcase
def b2 = "\xC3\xA9ab".rindex("a", 1).to_a
def b3 = "\u00e9"[1].upcase
def b4 = ("e" + "\xC3\xA9")[1].upcase

# --- (2) COVERAGE GAP: the oracle fires the true binary answer ---------------

# The fold declines, so these are silent on the port while the oracle names
# the constant it folded on the real bytes — gaps, never the wrong value.
def g1 = "h\xC3\xA9llo".index("l").lenght     # oracle: `lenght` for 3
def g2 = "h\xC3\xA9llo"[1].lenght            # oracle: `lenght` for "\xC3"
def g3 = "\xC3\xA9a"[1].lenght               # oracle: `lenght` for "\xA9"
def g4 = "\xC3\xA9ab".rindex("a", 1).lenght  # oracle: `lenght` for nil
def g5 = ("\xC3\xA9" <=> "a").to_a           # oracle: `to_a` for 1
def g6 = "abc".index("\xC3").lenght          # oracle: `lenght` for nil
y = "\xC3\xA9a".index("a") + 1               # oracle: `+` for 3, `to_a` for 3
y.to_a

# The inverted-verdict row: under binary `rindex` answers `nil`, so the
# oracle warns `always falsey`; the port declines instead of minting the
# wrong `always truthy`.
if "\xC3\xA9ab".rindex("a", 1)
  puts 1
end

# --- (3) FIRES: ASCII literals are encoding-proof ----------------------------

# Byte = char for ASCII under every script encoding, so these folds stay
# exact — the controls proving the gate only withholds non-ASCII scalars.
def c1 = "e"[1].upcase                     # `upcase` for nil
def c2 = "abc".rindex("z").lenght          # `lenght` for nil
def c3 = "abc".getbyte(9).to_a             # silent: `to_a` on nil exists
def c4 = "abc".byteindex("b").lenght       # `lenght` for 1

# --- (4) STAYS SILENT: the method-return fold declines too --------------------

# The round-3 gate one level up: the literal tail of a `def` is pinned at
# harvest time (`capture_fold_tail`), BEFORE any call site exists — so the
# call-site encoding check could not see it. A non-ASCII `Str`/`Sym` from a
# non-UTF-8 file now declines there as well: `Fold.b` cannot pin `nil` for a
# body Ruby reads as `"\xA9"`, and `Fold.s` cannot hand a caller the UTF-8
# text of a string Ruby holds as bytes. `Fold.b.upcase` and the `Fold.s`
# lookups all fired `for nil` on the port before this gate (the cross-file
# twin — a UTF-8 file calling these — is probe-only: a fixture is one file).
class Fold
  def self.b = "\xC3\xA9"[1]
  def self.s = "a\xC3\xA9"
  def self.a = "abc"
end
Fold.b.upcase                 # silent: declined body, `upcase` on String
Fold.s.rindex("a", -3).abs    # silent: declined pin, `abs` on flat Integer
Fold.s[-3].upcase             # silent: declined pin, `upcase` on flat String
Fold.s.index("a").abs         # silent: `abs` on flat Integer either way
Fold.s.slice(-3).upcase       # silent: `upcase` on flat String

# And the controls proving the decline is keyed on the scalar's bytes, not on
# the fold path existing: `Fold.a`'s ASCII literal still pins and still folds
# — byte = char under every script encoding.
Fold.a[3].upcase              # `upcase` for nil
Fold.a.rindex("z").lenght     # `lenght` for nil
