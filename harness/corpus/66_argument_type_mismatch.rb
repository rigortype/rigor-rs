# call.argument-type-mismatch (ADR-64) — fixture 66.
#
# Exercises the firing channels rigor-rs and the reference AGREE on (parity is
# keyed on (rule, line, column)), plus the zero-FP skip cases the rule must stay
# silent on. The nilable-local / nilable-method-return channels the reference
# ALSO fires (a `String?` argument) are NOT exercised here: rigor-rs's `type_of`
# does not carry flow-narrowed local nilability nor a nilable RBS return into an
# argument position, so those stay coverage gaps (documented, FP-safe).

require "base64"

# --- nil channel, single overload (String#+ param `string` rejects nil) -------
"abc" + nil

# --- nil channel, multi overload (Integer#+ — no numeric overload admits nil) -
5 + nil

# --- non-nil channel, multi overload (Array#fetch index param `int` rejects a
#     concrete String argument) ------------------------------------------------
[1, 2, 3].fetch("x")

# --- non-nil channel, singleton/class method (Base64.urlsafe_decode64's `String`
#     param rejects a concrete Integer argument) --------------------------------
Base64.urlsafe_decode64(42)

# --- nil channel via a folded Hash-literal miss (h["z"] folds to nil) ---------
h = { "a" => 1 }
Base64.urlsafe_decode64(h["z"])

# --- SKIP: universal-equality methods accept any argument (never fires) -------
name = "a"
name == nil
name.eql?(nil)

# --- SKIP: coerce-dispatch operator on a multi-overload method (5 + "s" is
#     valid via `String#coerce`-style dispatch; the non-nil channel excludes it)
5 + "s"

# --- SKIP: an interface-alias param (`int`) is degraded to gradual, so a
#     concrete non-nil argument the alias would reject stays silent ------------
"abc".center("s")

# --- SKIP: a splat / keyword argument makes the call non-plain-positional -----
def forward(parts)
  "abc".center(*parts)
end
