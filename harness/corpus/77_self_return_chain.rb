# An RBS `-> self` return on an INSTANCE method — "the receiver itself", the
# spelling core uses for the mutating and re-tagging families (`Array#concat`,
# `Array#push`, `String#force_encoding`, `Object#freeze`, …). The port tracked
# the late-bound return only on the SINGLETON path (`Time.now: () -> instance`),
# so an instance method's `-> self` collapsed to `Dynamic` and every method
# chained off it went unwitnessed.
#
# Measured on rigor-survey: gitlab-foss
# `lib/gitlab/database/queue_error_handling_concern.rb:28` —
# `[error.message].concat(error.backtrace).join("\n").truncate(N)` — where the
# `concat` broke the chain and the ActiveSupport-only `truncate` went unreported.

# --- Array#concat: (*array[Elem]) -> self ------------------------------------
parts = ["a"].concat(["b"]).join("-")
parts.frobnicate

# --- String#force_encoding: (encoding) -> self -------------------------------
buf = "abc".force_encoding("UTF-8")
buf.frobnicate

# --- Array#push: (*Elem) -> self ---------------------------------------------
nums = [1, 2].push(3)
nums.frobnicate

# --- NEGATIVE CONTROL: a SINGLETON `-> self` is the class OBJECT, not an
# instance of it, and the flat return slot cannot spell that — so it must keep
# declining to `Dynamic` rather than being folded like the instance case.
# `Struct.new` returns a Class; a method call on it stays unwitnessed.
Struct.new(:x).definitely_absent_zzz
