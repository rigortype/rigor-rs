# v0.4.6 review declines (#2269 / #2237 / #2094 / #2225).
#
# #2269: a class object's extended module answers through its own
# include/prepend closure too (level 0 of the singleton chain): a project
# module it includes, an RBS module (`Comparable#between?`), or an include the
# project cannot name (which opens every name on that class).
# #2237: a `define_method` / `alias_method` / `alias` inside a
# `K.class_eval { … }`-family block outside K's body (top level, another class
# body, a qualified receiver, after a `private` in the block) replaces `K#x`,
# so the reduced-visibility `def` no longer reports.
# #2094: a macro call inside a mixed-in module's `class << self` names the
# method on both sides.
# #2225: an `alias_method` / `define_method` in `K.class_eval { … }` written
# after `def x` supersedes the def's return type.
#
# Must-still-fire controls: a name no extended module answers
# (`Rational.not_named`, `Complex.not_named`), an untouched class
# (`Integer.zzz_i`), an `instance_eval` / in-`def` / receiver-form
# `define_method` (K15 / K16 / K17), a macro module's unnamed method
# (`"s".not_named`), and a class_eval that replaces another name (R4).

module Inner149
  def zork149 = 1
end
module Ext149
  include Inner149
end
Rational.extend(Ext149)
Rational.zork149
Rational.not_named

module Cmp149
  include Comparable
end
Complex.extend(Cmp149)
Complex.between?(1, 2)
Complex.not_named

module Open149
  include SomeGem149::Unknown
end
Random.extend(Open149)
Random.anything149

Integer.zzz_i

class P149
  def x = 1
  def y = 1
end
class K11 < P149
  private def x = 2
end
K11.class_eval { define_method(:x) { 3 } }
class K12 < P149
  private def x = 2
end
K12.module_eval { alias_method :x, :y }
class K13 < P149
  private def x = 2
end
K13.class_exec { alias x y }
class K14 < P149
  private def x = 2
end
class Other149
  K14.class_eval do
    private
    define_method(:x) { 3 }
  end
end
module A149
  class K18 < P149
    private def x = 2
  end
end
A149::K18.class_eval { define_method(:x) { 3 } }

class K15 < P149
  private def x = 2
end
K15.instance_eval { define_method(:x) { 3 } }
class K16 < P149
  private def x = 2
end
def setup149 = K16.class_eval { define_method(:x) { 3 } }
class K17 < P149
  private def x = 2
end
K17.define_method(:x) { 3 }

module Mac149
  class << self
    my_macro149 :zork_m
  end
end
class String
  include Mac149
end
"s".zork_m
"s".not_named
module SMac149
  class << self
    my_macro149 :zork_s
  end
end
Float.extend(SMac149)
Float.zork_s

class R1
  def x = 1
end
n = :x
R1.class_eval { alias_method n, :to_s }
R1.new.x.upcase
class R2
  def x = 1
end
R2.class_eval { alias_method :x, :to_s }
R2.new.x.upcase
class R3
  def x = 1
end
R3.class_eval { define_method(:x) { "s" } }
R3.new.x.upcase
class R4
  def x = 1
end
R4.class_eval { define_method(:y) { "s" } }
R4.new.x.upcase
