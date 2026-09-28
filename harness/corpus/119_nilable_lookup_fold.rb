# `String#getbyte` / `#rindex` / `#byteindex` / `#byterindex` and `Float#<=>`
# fold on literals (issue #164), joining fixture 113's String lookup family.
#
# The reference folds each by calling the method on the constants
# (`constant_folding.rb`: STRING_BINARY / NUMERIC_BINARY, the `leaf` catalog
# rows for the ternary forms). A fold to `nil` makes a method `nil` has
# (`to_a`) silent and names `nil` on a typo; the port used to answer the flat
# slot's bare `Integer` on every row. A literal call the port cannot fold
# (multibyte receiver, Float index, a raising argument) declines to silence.
#
# Every row is oracle-measured at the pin, one fresh temp cwd per case,
# `--no-cache`, both reference libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- (1) SILENT: the fold is nil, and nil has `to_a` -------------------------
"abc".getbyte(9).to_a
"abc".getbyte(-9).to_a
"abc".rindex("z").to_a
"abc".byteindex("z").to_a
"abc".byterindex("z").to_a
(1.0 <=> "x").to_a

# --- (2) FIRES `for nil`: the fold is nil ------------------------------------
"abc".getbyte(9).lenght
"abc".rindex("a", -4).lenght
"abc".byteindex("b", 5).lenght
(1.0 <=> :x).lenght
(1.0 <=> nil).lenght

# --- (3) FIRES on the folded constant (was `for Integer`) --------------------
"abc".rindex("b").to_a
"abc".byteindex("b").to_a
"abc".byterindex("b").to_a
"abc".getbyte(0).to_a
(1.0 <=> 2).to_a
"abc".rindex("c", -1).lenght
"abcabc".rindex("bc", 3).lenght
"abc".byteindex("c", -1).lenght
"abc".byterindex("", 5).lenght
(2.0 <=> 2).lenght
(1.5 <=> 1.0).lenght
(9007199254740992.0 <=> 9007199254740993).lenght

# --- (4) SILENT: a mutated local must not fold on its stale first literal ----
x = "abc"
x << "d"
x.getbyte(3).to_a
x.getbyte(3).lenght

# --- (5) SILENT: Ruby raises, so the reference declines to `Integer | nil` ---
"abc".getbyte("a").lenght
"abc".rindex(:b).lenght
