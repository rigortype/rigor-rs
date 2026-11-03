# Upstream v0.4.2 (rigor#1808, with #1736/#1742): a call's keyword hash now
# picks the overload that declares those keywords instead of the first
# positional reader. `Integer(s, exception: false)` and its `Float` /
# `Rational` twins take the `(…, exception: bool) -> T?` arm and answer
# `T | nil`, on which `call.undefined-method` stays silent; through v0.4.1 the
# first overload's `T` fired. Only the literal `exception: true` keeps the
# raising arm.
#
# Every row is oracle-measured at the `v0.4.2` pin.

Integer("1", exception: false).zork_a
Float("1.5", exception: false).zork_c
Integer("ff", 16, exception: false).zork_d
Rational("1", exception: false).zork_e
flag = false
Integer("1", exception: flag).zork_g
Integer("abc", exception: false).zork_i

# MUST STILL FIRE: the raising arm.
Integer("1", exception: true).zork_b
Integer("1").zork_ctl
Float("1.5").zork_ctl2
