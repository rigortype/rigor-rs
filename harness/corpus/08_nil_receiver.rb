# Expected reference diagnostics:
#   (call.undefined-method, line 2, column 3): undefined method `upcase' for nil
#
# rigor-rs status: COVERAGE GAP — nil receiver type not yet tracked in rigor-rs type lattice
x = nil
x.upcase
