# Two project sig/ declarations of one member (pre-existing; reported at
# v0.4.7): `RBS::DuplicatedMethodDefinitionError` fails that class's build, so
# the reference reads it `Dynamic[top]` and witnesses nothing on it. Keys are
# per side: a duplicated instance `def` leaves the singleton side buildable.
# An overloading `def m: ... | ...` composes and is not a duplicate.
#
# Must-still-fire controls: `Bar` (no duplicate), `Ovl` (overload continuation).
class Foo
  def a = 1
end
class Bar
  def b = 1
end
class Ovl
  def o = 1
end
class Attr; end

Foo.new.zork
Foo.new.a.zork
Bar.new.zork
Ovl.new.zork
Attr.new.zork
