# A non-`def` definer on the class's own singleton class replaces an earlier
# `def self.x` (upstream #1936, and #2045 for a conditional or non-literal
# one, v0.4.5): `singleton_class.alias_method` / `define_method` /
# `attr_reader`, `define_singleton_method`, the `class << self` spellings
# and `alias`, and a same-file reopening. The `def self.x` literal no longer
# types the call; at v0.4.4 every row below fired `undefined method 'even?'
# for "s"`.
#
# Must-still-fire controls: a `def self.x` after the alias wins again, a
# rewrite of another name, `attr_writer` (only `x=`), and an untouched class.

class RwAlias; def self.x = "s"; def self.y = 1; singleton_class.alias_method :x, :y; end
RwAlias.x.even?
class RwDefine; def self.x = "s"; singleton_class.define_method(:x) { 1 }; end
RwDefine.x.even?
class RwDsm; def self.x = "s"; define_singleton_method(:x) { 1 }; end
RwDsm.x.even?
class RwSclassAttr; def self.x = "s"; class << self; attr_reader :x; end; end
RwSclassAttr.x.even?
class RwSclassAlias; def self.x = "s"; def self.y = 1; class << self; alias x y; end; end
RwSclassAlias.x.even?
class RwCond; def self.x = "s"; def self.y = 1; singleton_class.alias_method :x, :y if ENV["Q"]; end
RwCond.x.even?
class RwNonLit; def self.x = "s"; def self.z = "z"; def self.y = 1; n = :x; singleton_class.alias_method n, :y; end
RwNonLit.z.even?
class RwReopen; def self.x = "s"; end
class RwReopen; singleton_class.alias_method :x, :to_s; end
RwReopen.x.even?

# controls
class RwLater; def self.y = 1; singleton_class.alias_method :x, :y; def self.x = "later"; end
RwLater.x.even?
class RwOther; def self.x = "s"; def self.y = 1; singleton_class.alias_method :o2, :y; end
RwOther.x.even?
class RwWriter; def self.x = "s"; singleton_class.attr_writer :x; end
RwWriter.x.even?
class RwPlain; def self.x = "s"; end
RwPlain.x.even?
