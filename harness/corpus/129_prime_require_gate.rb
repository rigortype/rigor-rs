# Upstream v0.4.2 (rigor#1700/#1701, `Environment::RequiredFeatures`): a
# `require "prime"` anywhere in the analysed files loads the vendored prime
# signatures, which declare `Integer#prime?`, `#prime_division`,
# `Integer.each_prime` and `.from_prime_division`, so those calls stop
# reporting `call.undefined-method`. Without the require (or with it only in
# a whole-line comment) the reference keeps firing on them; that half is the
# Rust test in rigor-parse plus the probes recorded on the PR, since one
# fixture file cannot hold both a require and its absence.
#
# Every row is oracle-measured at the `v0.4.2` pin.

require "prime"

12.prime?
Integer.each_prime(10) { |p| p }
Integer.from_prime_division([[2, 2]])
Prime.first(3)

# MUST STILL FIRE: a name prime does not declare.
12.zork_ctl
Integer.zork_ctl2
