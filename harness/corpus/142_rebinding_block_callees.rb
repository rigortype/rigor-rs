# A block handed to a callee that may run it under another `self` is not a
# top-level scope (upstream v0.4.6, #1817 / #2183 / #2184 / #2186,
# `RebindingBlockCallees`).
#
# `call.unresolved-toplevel` declines inside a block whose callee is a core
# rebinder (`instance_exec`, `define_method`, ...), a project method whose
# body hands the block on, a `send` with a computed name, or (when the callee
# does not resolve on its receiver) any name once a project `method_missing`
# hands its block on. A proc local passed as `&blk` is followed to its call.
#
# Must-still-fire controls: a block on a receiver whose method resolves
# (`[1].each`, `puts`, `loop`), a callee whose project body only yields, a
# proc called in place, and every call outside a block.

class Forwarder
  def method_missing(name, *args, &block)
    return 1 if name == :x

    super
  end

  def respond_to_missing?(name, include_private = false) = name == :x || super
end

module Ext
  def self.me(&b) = module_eval(&b)
  def self.yields = yield
end

RSpec.describe "spec" do
  before(:each) { helper_a }
  it "runs" do
    helper_b
  end
end
Ext.me { helper_c }
blk = proc { helper_d }
Ext.me(&blk)
Object.new.instance_exec { helper_e }
define_method(:defined) { helper_f }

[1].each { helper_g }
puts("x") { helper_h }
loop { helper_i }
proc { helper_j }.call
helper_k
