# Expected reference diagnostics:
#   (call.undefined-method, line 2, column 12): undefined method `lenght' for "hello"
#
# rigor-rs status: SUPPORTED — chained call; receiver type flows through String#downcase -> String
s = "Hello"
s.downcase.lenght
