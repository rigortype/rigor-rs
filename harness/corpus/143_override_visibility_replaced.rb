# A `define_method` or alias of the name gives a reduced override another
# visibility (upstream v0.4.6, #2237 / issue #2013): the slot is contested,
# so `def.override-visibility-reduced` proves no reduction, whether the
# replacer is later in the body or under a conditional.
#
# Must-still-fire controls: a reduction with no replacer, and one whose
# replacer a later `private :x` writes again.

class Parent
  def x = 1
  def y = 3
end

class ByDefineMethod < Parent
  private def x = 2
  define_method(:x) { 3 }
end

class ByAliasMethod < Parent
  private def x = 2
  alias_method :x, :y
end

class ByAlias < Parent
  private def x = 2
  alias x y
end

class ByConditional < Parent
  private def x = 2
  define_method(:x) { 3 } if ENV["X"]
end

class Plain < Parent
  private def x = 2
end

class Settled < Parent
  private def x = 2
  define_method(:x) { 3 }
  private :x
end
