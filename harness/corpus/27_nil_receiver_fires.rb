# Expected reference diagnostics:
#   (call.possible-nil-receiver, line 7, column 5): possible nil receiver: `upcase' is undefined on NilClass
#
# rigor-rs status: PARITY — the nilable-RBS-return slice fires here.
#
# A nilable core RBS return (`String#byteslice -> String?`) on a NON-constant
# Nominal receiver (`s : String` via `String.new`) mints `String | nil`. `upcase`
# is present on String (the non-nil arm) but absent on NilClass, and no guard
# narrows nil away ⇒ both tools fire on the `upcase` call (byte-exact message,
# error severity under the default balanced profile).
def fetch
  s = String.new
  x = s.byteslice(0, 2)
  x.upcase
end
