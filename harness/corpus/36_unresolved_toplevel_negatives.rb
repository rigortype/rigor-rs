# Adversarial FP guard for call.unresolved-toplevel: every DECLINE case. None of
# the toplevel implicit-self calls below may emit an unresolved-toplevel
# diagnostic on rigor-rs OR the reference. An extra here would be a false
# positive — and this rule's soundness rests on resolving the FULL Object/Kernel
# surface (all `def self?.x` in core RBS) plus same-file toplevel defs.

# Kernel / Object methods (declared `def self?.x` in core RBS ⇒ recorded as
# instance methods on Kernel, which Object includes) all resolve ⇒ silent.
puts "a"
print "b"
p "c"
require "set"
require_relative "sibling"
loop { break }
raise "boom" rescue nil
warn "w"
proc { 1 }
lambda { 2 }
srand
rand(10)
format("%d", 1)
sprintf("%s", "x")
caller
block_given?
at_exit { 0 }
catch(:done) { throw :done }
freeze
gets

# A same-file toplevel `def` resolves a toplevel call to it ⇒ silent.
def local_helper
  42
end
local_helper

# Implicit-self calls inside a class/module body (even unresolved) are NOT
# toplevel ⇒ silent (ADR-24 leniency).
module Outer
  module_level_macro
  class Inner
    class_level_macro
    def instance_method
      call_inside_method
    end
  end
end
