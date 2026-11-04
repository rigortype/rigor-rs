# An `alias_method` whose names cannot be read (upstream v0.4.6, #2225 /
# issue #2006, `unread_alias_method?`) still replaces a method: every method
# its class wrote before it is contested, so its `def` no longer types calls.
#
# Must-still-fire controls: a method written after the alias, and an
# `alias_method` with two literal names, which only replaces the named one.

class Unread
  def x = 1
  def y = 2
  n = :x
  alias_method n, :to_s
  def z = 3
end
Unread.new.x.upcase
Unread.new.y.upcase
Unread.new.z.upcase

class Literal
  def x = 1
  def w = 2
  alias_method :w, :to_s
end
Literal.new.x.upcase
