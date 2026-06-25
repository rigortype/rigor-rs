# Expected reference diagnostics:
#   (call.wrong-arity, line 2, column 3): wrong number of arguments to `gsub' on String (given 3, expected 1..2)
#
# rigor-rs status: COVERAGE GAP — call.wrong-arity not yet implemented in rigor-rs
s = "hello world"
s.gsub("a", "b", "c")
