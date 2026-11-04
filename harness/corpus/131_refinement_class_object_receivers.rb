# Upstream v0.4.3 (#1836, rigor #1752): a refinement of `Object`, `Module`,
# `Class`, `Kernel` or `BasicObject`, or of a module a class extends, reaches
# a call on a class or module object under the refining module's `using`,
# which then types `Dynamic` and reports nothing. A class object's lookup
# reaches `Class` only for a class, so a `refine(Class)` name still reports on
# a module object.
#
# Every row is oracle-measured at the `v0.4.3` pin (v0.4.2 fired each
# retracted row). The MUST-STILL-FIRE rows sit before the `using`, call a
# name nothing refines, or put a `Class` name on a module object.

module CoRefine
  refine(Object) { def co_oo = :o }
  refine(Module) { def co_mm = 1; def co_label = name }
  refine(Class) { def co_cc = "c" }
  refine(Kernel) { def co_kk = 3.0 }
  refine(BasicObject) { def co_bo = [1] }
end

# MUST STILL FIRE: before the `using`.
Integer.co_oo

using CoRefine

Integer.co_oo
Comparable.co_oo
Comparable.co_mm
Integer.co_label
Integer.co_cc
Integer.co_kk
Comparable.co_kk
Integer.co_bo

# MUST STILL FIRE: nothing refines the name; `Class` is not on a module
# object's lookup.
Integer.co_nothere
Comparable.co_cc
5.co_nothere
