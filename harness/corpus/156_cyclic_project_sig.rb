# A cyclic superclass or mixin chain in the project's sig/ (upstream v0.4.7,
# #2573): RBS raises `RecursiveAncestorError`, the classes on the cycle (and
# their subclasses / includers) read `Dynamic[top]`, so nothing is witnessed on
# them. A diamond (`Both` includes `M3` twice through `M4`) stays buildable.
#
# Must-still-fire controls: `C` (no cycle), `Both` (diamond) and a core receiver.
class A < B
  def a = 1
end
class B < A; end
class C
  def c = 1
end
class Self
  def s = 1
end
class UsesM; end
class Sub < Self; end
class Both; end

A.new.zork
B.new.zork
C.new.zork
Self.new.zork
UsesM.new.zork
Sub.new.zork
Both.new.zork
1.zork
