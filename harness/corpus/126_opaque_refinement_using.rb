# Upstream v0.4.2 (ADR-121 WD7, rigor#1796): a `using M` whose module the
# project does not declare (a gem's, or one from outside the analysed paths)
# puts refinements in effect that nothing can read; they may redefine any
# method of any class. So `call.undefined-method`, `call.wrong-arity`,
# `call.argument-type-mismatch`, `call.possible-nil-receiver`,
# `call.unresolved-toplevel` and `flow.always-raises` all decline for the rest
# of that lexical scope: a class body's `using` ends with the class, a
# top-level one with the file. A project module's `using` is not opaque (its
# refinements are read; the port, which models none, keeps firing there).
#
# Every row is oracle-measured at the `v0.4.2` pin. The MUST-STILL-FIRE rows
# sit before the `using`, outside the class that writes one, or after a
# project module's.

module OuProjectRefine
  refine(String) { def ou_project_zz = 1 }
end

# MUST STILL FIRE: before any `using`.
"s".ou_nope_before
1.succ(2)
ou_toplevel_before

class OuScoped
  # MUST STILL FIRE: ahead of the class body's `using`.
  def m1 = "s".ou_nope_m1

  using SomeGem::Refinements

  def m2 = "s".ou_nope_m2
  def m3 = 1.succ(3)
  def m4 = 1.gcd(:x)
end

class OuOther
  # MUST STILL FIRE: the class body's `using` ended with `OuScoped`.
  def m5 = "s".ou_nope_m5
end

using OuProjectRefine

# MUST STILL FIRE: a project module's refinements are readable.
"s".ou_nope_after_project
1.succ(4)

using Another::Gem::Refinements

"s".ou_nope_after
1.succ(5)
1.gcd(:y)
ou_toplevel_after
[nil, 1].first.succ
1 / 0
