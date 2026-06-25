# Expected reference diagnostics:
#   (call.wrong-arity, line 2, column 3): wrong number of arguments to `include?' on String (given 2, expected 1)
#
# rigor-rs status: COVERAGE GAP — call.wrong-arity not yet implemented in rigor-rs
s = "Hello"
s.include?("e", "x")
