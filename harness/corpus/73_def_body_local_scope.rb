# A `def` body is an independent LOCAL scope: it never sees the file's
# top-level locals, so a parameter that shares a name with one is a DIFFERENT
# variable and must not inherit its type.
#
# Prism already encodes this for a bare name (`s` inside a def that does not bind
# `s` lowers to a CALL, see 35_unresolved_toplevel). What it cannot encode is the
# name COLLISION below: `s` in `describe_it` is the parameter, but rigor-rs typed
# every use site against one flat name-keyed top-level env and so read `"anagram"`
# — the shape of rigor-survey `Ruby/data_structures/hash_table/anagram_checker.rb`,
# which runs a driver section and then reopens the method it was driving.

s = "anagram"
t = ["a", "b"]

# The driver section itself IS top-level, so top-level locals still type here:
# a typo on `s` fires, exactly as before.
s.lenght

def describe_it(s, t)
  # `s` and `t` are the parameters — untyped, so NOTHING may fire on them. Typed
  # against the top-level env they would read String / Array and report
  # `undefined method 'each'` and a wrong arity.
  s = s.chars
  s.each { |c| c }
  t.count
  s.count
end

# A block, unlike a def, DOES capture the enclosing locals — the top-level env
# must still reach here.
[1].each do |_n|
  s.lenght
end
