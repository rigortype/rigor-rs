# Expected reference diagnostics:
#   (call.undefined-method, line 16): undefined method `lenght' for "hi"
#
# rigor-rs status: SUPPORTED — ADR-0023 tier-4b call-site PARAMETER BINDING.
# `full`'s tail is a bare read of positional param `x`, so the call site binds
# the ARGUMENT's type (`"hi"` : String) to the param and re-derives the return
# (String). The chained `.lenght` then witnesses against the real String RBS.
# The reference witnesses the same call (its receiver render is the value `"hi"`;
# rigor-rs renders the class `String` — same class, the pre-existing literal-vs-
# nominal render gap, NOT a new diagnostic). A strict subset of the reference.
class Greeter
  def full(x)
    x
  end
end
g = Greeter.new
g.full("hi").lenght
