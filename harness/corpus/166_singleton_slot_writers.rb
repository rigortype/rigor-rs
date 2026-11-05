# Issue #476: the reference's singleton def table is last-writer-wins, and a
# `def self.x` site loses its slot to a later `module_function` copy, a `def`
# in an `instance_eval` block, or a `def self.x` the reference writes as
# possible (under an `if`, in a block). A `class << self` nested in a
# `class << self` opens the singleton's own singleton. None of these may
# fold `x` to the first site's "s".
#
# Must-still-fire controls: a plain site, a writer before the site, and a
# writer of another name.
module MfBare
  def self.x = "s"
  module_function
  def x = 1
end
MfBare.x.even?

module MfNamed
  def self.x = "s"
  def x = 1
  module_function :x
end
MfNamed.x.even?

module IeSelf
  def self.x = "s"
  instance_eval { def x = 1 }
end
IeSelf.x.even?

module IfDef
  def self.x = "s"
  if $c
    def self.x = 1
  end
end
IfDef.x.even?

module BlockDef
  def self.x = "s"
  [1].each { def self.x = 1 }
end
BlockDef.x.even?

module NestedSclass
  class << self
    class << self
      def x = "s"
    end
  end
end
NestedSclass.x.even?

module Plain
  def self.x = "s"
end
Plain.x.even?

module MfBefore
  module_function
  def x = 1
  def self.x = "s"
end
MfBefore.x.even?

module MfReset
  def self.x = "s"
  module_function
  public
  def x = 1
end
MfReset.x.even?

module IeOther
  def self.x = "s"
  instance_eval { def y = 1 }
end
IeOther.x.even?
