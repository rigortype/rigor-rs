# Issue #479, item 3: `extend` inside a receiver `instance_eval` block.
#
# The reference's extends walk (`walk_eval_extends_call`) opens an
# `instance_eval` / `instance_exec` block as the receiver's own body, as it
# opens `class_eval`. So `X.instance_eval { extend M }` files `M` on `X`'s
# singleton side, as `X.extend(M)` does. The port's census frame for that block
# owned nothing, so the singleton-mixin decline never saw `M`. A `self.extend M`
# names no class in `receiver_extend_recorded?`, so it is a taint (the
# wildcard).

module Ext479
  def zork = 1
end

Rational.instance_eval { extend Ext479 }
Rational.zork
Rational.not_named          # control: Ext479 does not define it

Complex.instance_exec do
  singleton_class.include Ext479
end
Complex.zork
Complex.not_named           # control

Float.instance_eval { class << self; include Ext479; end }
Float.zork
Float.not_named             # control

Integer.class_eval { self.extend(Ext479) }
Integer.not_named           # a `self.` receiver taints: silent on both

Symbol.instance_eval { include Ext479 }
Symbol.zork                 # control: an include is not a singleton mixin
