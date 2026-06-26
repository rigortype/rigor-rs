# Expected reference diagnostics:
#   (call.undefined-method, line 7, column 3): undefined method `lenght' for "x"
#
# rigor-rs status: SUPPORTED — in-source line suppression (# rigor:disable)
# The L8 typo is silenced by its trailing disable comment; only L7 survives.
s = "x"
s.lenght
s.bogus # rigor:disable undefined-method
