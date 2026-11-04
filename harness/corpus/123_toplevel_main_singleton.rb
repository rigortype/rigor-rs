# Upstream v0.4.2 (rigor#1683 / issue #1383; rigor#1723 / issue #1716).
#
# (1) A bare top-level `using`, `include`, `define_method`, `public`,
# `private` or `ruby2_keywords` is a call to one of `main`'s private singleton
# methods. The reference reads the set off RBS core's
# `RBS::Unnamed::TopLevelSelfClass` (plus `ruby2_keywords`, which RBS does not
# declare), and judges a call inside a top-level block the same way. The
# MUST-STILL-FIRE rows are names `main` does not have. This file has no
# top-level `include` of a module (that is fixture 124's subject), only one
# inside a block, which the reference does not record as an `Object` mixin.
#
# (2) `def.override-visibility-reduced` cannot prove a reduction on a name
# CRuby makes private on every non-singleton `def` (`initialize`, the copy
# hooks, `respond_to_missing?`). A plain method keeps firing.
#
# Every row is oracle-measured at the `v0.4.2` pin.

module TlRefine
  refine(String) { def tl_zz = 1 }
end

# --- (1) main's singleton methods: silent --------------------------------
using TlRefine
define_method(:tl_defined) { 1 }
def tl_kw(*a) = a
ruby2_keywords(:tl_kw)
public :tl_kw
[1].each { include Comparable }
private

# MUST STILL FIRE: not on main.
tl_nope_toplevel
extend_object_zz(1)
[1].each { tl_nope_in_block }

# --- (2) implicitly private overrides -------------------------------------
class TlParent
  def initialize = nil
  def initialize_copy(o) = o
  def initialize_dup(o) = o
  def initialize_clone(o) = o
  def respond_to_missing?(n, p = false) = false
  def tl_plain = 1
end

class TlChild < TlParent
  private

  def initialize = nil
  def initialize_copy(o) = o
  def initialize_dup(o) = o
  def initialize_clone(o) = o
  def respond_to_missing?(n, p = false) = false
  # MUST STILL FIRE: a plain reduction.
  def tl_plain = 2
end
