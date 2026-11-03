# Upstream v0.4.2 (rigor#1691): Ruby 4.1's String bit operations (`bit_set`,
# `bit_clear`, `bit_flip`, `bitwise_not!`, `bitwise_and!`, `bitwise_or!`,
# `bitwise_xor!`), which the vendored `core_overlay/string.rbs` declares
# whatever Ruby runs the analysis, rewrite the receiver in place. They join
# `StringMutation::MUTATORS`, so a literal-bound local or constant widens to
# `String` after one and stops folding to its literal.
#
# Every row is oracle-measured at the `v0.4.2` pin.

t = "ab"
t.bit_flip(1)
t.frobnicate_t

w = "ab"
w.bitwise_and!("\x0F")
w.upcase.quux_w

FOO = +"ab"
FOO.bit_set(0)
FOO.zork_foo

BAR = "ab"
BAR.bit_flip(1) if BAR == "ab"
BAR.zork_bar

# MUST STILL FIRE: no mutator ran on this binding.
x = "ab"
x.frob_ctl
