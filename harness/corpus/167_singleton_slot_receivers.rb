# Issue #476: singleton slot writers named through a constant receiver
# (`C.instance_eval { def x }`, `C.class_eval { def self.x }`,
# `C.singleton_class.class_eval`) and `def C.x` in C's own body contest the
# `def self.x` site, wherever they are written in the file.
#
# Must-still-fire controls: a top-level `define_singleton_method` (the
# reference reads it only on `self`), an instance-side `class_eval` def, and
# a writer that runs before the site.
module TopIe
  def self.x = "s"
end
TopIe.instance_eval { def x = 1 }
TopIe.x.even?

module TopCe
  def self.x = "s"
end
TopCe.class_eval { def self.x = 1 }
TopCe.x.even?

module TopScCe
  def self.x = "s"
end
TopScCe.singleton_class.class_eval { def x = 1 }
TopScCe.x.even?

module OwnPath
  def self.x = "s"
  def OwnPath.x = 1
end
OwnPath.x.even?

module Outer
  def self.x = "s"
  class Inner
    Outer.instance_eval { def x = 1 }
  end
end
Outer.x.even?

module TopDsm
  def self.x = "s"
end
TopDsm.define_singleton_method(:x) { 1 }
TopDsm.x.even?

module InstCe
  def self.x = "s"
end
InstCe.class_eval { def x = 1 }
InstCe.x.even?

module IeFirst; end
IeFirst.instance_eval { def x = 1 }
module IeFirst
  def self.x = "s"
end
IeFirst.x.even?
