# A parent's visibility across reopened bodies in ONE file (issue #473).
#
# The reference keeps one visibility table per file, written in walk order, so
# a later reopening's write wins, and a named re-mark in a reopening
# (`private :foo`, `private attr_reader :foo`, `module_function :foo`) re-marks
# the slot an earlier body recorded. The port merged the bodies first-write-wins
# and fired "public to protected" on every silent row below.
#
# Must-still-fire controls: a later public reopening of a private parent, a
# `public :foo` re-mark, a parent reopened without touching `foo`, and a
# `protected :foo` re-mark under a private child.

# Silent: the later wrapped `private def` wins.
class BaseA; def foo = 1; end
class BaseA; private def foo = 2; end
class SubA < BaseA
  protected
  def foo = 3
end

# Silent: the later bare `private` section wins.
class BaseB; def foo = 1; end
class BaseB
  private
  def foo = 2
end
class SubB < BaseB
  protected
  def foo = 3
end

# Silent: a named re-mark in a reopening.
class BaseC; def foo = 1; end
class BaseC; private :foo; end
class SubC < BaseC
  protected
  def foo = 3
end

# Silent: an attr re-mark in a reopening.
class BaseD; def foo = 1; end
class BaseD; private attr_reader :foo; end
class SubD < BaseD
  protected
  def foo = 3
end

# Silent: a module parent re-marked by `module_function`.
module ModE; def foo = 1; end
module ModE; module_function :foo; end
class SubE
  include ModE
  protected
  def foo = 3
end

# Silent: three bodies, the last one private.
class BaseF; def foo = 1; end
class BaseF; def foo = 2; end
class BaseF; private def foo = 3; end
class SubF < BaseF
  protected
  def foo = 4
end

# Fires: a later public reopening of a private parent.
class BaseG; private def foo = 1; end
class BaseG; def foo = 2; end
class SubG < BaseG
  protected
  def foo = 3
end

# Fires: `public :foo` in a reopening.
class BaseH
  private
  def foo = 1
end
class BaseH; public :foo; end
class SubH < BaseH
  protected
  def foo = 3
end

# Fires: a reopening that does not touch `foo`.
class BaseI; def foo = 1; end
class BaseI; private; def bar = 1; end
class SubI < BaseI
  protected
  def foo = 3
end

# Fires: `protected :foo` in a reopening, then a private child.
class BaseJ; def foo = 1; end
class BaseJ; protected :foo; end
class SubJ < BaseJ
  private
  def foo = 3
end
