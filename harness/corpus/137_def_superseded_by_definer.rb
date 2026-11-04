# A later non-`def` definer in the same file replaces an earlier `def` for
# typing (upstream #2003, v0.4.5): `attr_reader` / `attr_accessor`, a
# literal `define_method`, `alias_method` and `alias` (the new name), also
# written `self.attr_reader` or `send(:attr_reader, ...)` (#2120), and a
# same-file reopening. The `def` body no longer types the call; at v0.4.4
# every row below fired `undefined method 'upcase' for 1`.
#
# Must-still-fire controls: a `def` after the definer wins again,
# `attr_writer` replaces only `x=`, a non-literal `define_method` names
# nothing, and a definer inside a method body is not on the class body.

class SupReader
  def x = 1
  attr_reader :x
end
SupReader.new.x.upcase

class SupAccessor
  def x = 1
  attr_accessor :x
end
SupAccessor.new.x.upcase

class SupDefine
  def x = 1
  define_method(:x) { "d" }
end
SupDefine.new.x.upcase

class SupAliasMethod
  def y = "y"
  def x = 1
  alias_method :x, :y
end
SupAliasMethod.new.x.upcase

class SupAlias
  def y = "y"
  def x = 1
  alias x y
end
SupAlias.new.x.upcase

class SupSelfAttr
  def x = 1
  self.attr_reader :x
end
SupSelfAttr.new.x.upcase

class SupSendAttr
  def x = 1
  send(:attr_reader, :x)
end
SupSendAttr.new.x.upcase

class SupReopen
  def x = 1
end
class SupReopen
  attr_reader :x
end
SupReopen.new.x.upcase

# controls
class CtlDefAgain
  def x = 1
  attr_reader :x
  def x = 2
end
CtlDefAgain.new.x.upcase

class CtlWriterOnly
  def x = 1
  attr_writer :x
end
CtlWriterOnly.new.x.upcase

class CtlNonLiteral
  def x = 1
  n = :x
  define_method(n) { "d" }
end
CtlNonLiteral.new.x.upcase

class CtlInDef
  def x = 1
  def m = attr_reader(:x)
end
CtlInDef.new.x.upcase
