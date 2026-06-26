# A provable Integer ZeroDivisionError: `flow.always-raises` fires on each
# Integer division/modulo by a constant-zero divisor. Byte-for-byte against the
# oracle on (rule=flow.always-raises, line, column = the operator/method token).
# The receiver is Integer-rooted (literal or folded local) and the divisor is a
# constant Integer zero on every line below.
x = 5
x / 0
10 % 0
7.div(0)
8.modulo(0)
9.divmod(0)
