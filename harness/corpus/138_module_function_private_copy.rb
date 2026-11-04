# A module function's instance copy is private (upstream #2055, v0.4.5): the
# reference's visibility table applies a `module_function` call as a
# `private` one, bare or named, so a private override of it reduces nothing.
# At v0.4.4 the rows below fired `def.override-visibility-reduced`.
#
# Must-still-fire controls: a plain public parent, `public def` inside the
# mode, and a bare `public` that ends the mode.

module MfBare
  module_function

  def fmt(value) = value.to_s
end
class MfBareUser
  include MfBare

  private

  def fmt(value) = "r#{value}"
end

module MfNamed
  def fmt = 1
  module_function :fmt
end
class MfNamedUser; include MfNamed; private def fmt = 2; end

module MfProt
  module_function
  def fmt = 1
end
class MfProtUser; include MfProt; protected def fmt = 2; end

# controls
module MfPlain
  def fmt = 1
end
class MfPlainUser; include MfPlain; private def fmt = 2; end

module MfPublicDef
  module_function
  public def fmt = 1
end
class MfPublicDefUser; include MfPublicDef; private def fmt = 2; end

module MfReset
  module_function
  public
  def fmt = 1
end
class MfResetUser; include MfReset; protected def fmt = 2; end
