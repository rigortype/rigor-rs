# Expected reference diagnostics:
#   (call.undefined-method, line 2, column 10): undefined method `nope' for "HELLO"
#
# rigor-rs status: SUPPORTED — type folds through String#upcase -> String constant "HELLO"
s = "Hello"
s.upcase.nope
