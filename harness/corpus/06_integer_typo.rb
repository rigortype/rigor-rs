# Expected reference diagnostics:
#   (call.undefined-method, line 2, column 3): undefined method `upcase' for 42
#
# rigor-rs status: SUPPORTED — integer literal receiver; Integer has no #upcase
n = 42
n.upcase
