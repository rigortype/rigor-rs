# Expected reference diagnostics:
#   (call.undefined-method, line 14, column 16): undefined method `lenght' for String
#
# rigor-rs status: SUPPORTED — ADR-0023 tier-4b in-source method RETURN inference.
# `full_name`'s tail is an interpolated String, typed under an EMPTY env to a
# concrete `String`, so `user.full_name : String` and the chained `.lenght`
# witnesses against the real String RBS (a strict subset of the reference).
class User
  def full_name
    "#{first} #{last}"
  end
end
user = User.new
user.full_name.lenght
