# Adversarial FP guard for flow.always-raises: every DECLINE case. This file
# must yield ZERO diagnostics on BOTH rigor-rs and the reference. If any
# always-raises fires here it is a false positive (an ERROR on correct code).

# 1. Non-zero divisor -> a valid division, never raises -> silent.
5 / 2

# 2. Float divisor (`0.0`) -> Float division by zero is Infinity, not an
#    error -> silent. (Receiver is Integer but the result is Float.)
5 / 0.0

# 3. Float receiver -> Float division -> Infinity, not an error -> silent.
5.0 / 0

# 4. Modulo by a non-zero constant -> silent.
10 % 3
