# Expected reference diagnostics: (none for call.undefined-method)
# Note: the reference may emit call.unresolved-toplevel (warning) for `x` —
# that rule is out of scope for this parity slice and is filtered out.
#
# rigor-rs status: SUPPORTED — dynamic/unknown receiver must be silent (zero-FP gate)
def foo(x)
  x.bar
end
