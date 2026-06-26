# def.override-visibility-reduced — positive: a subclass narrows an inherited
# public instance method to private across the superclass chain. The override
# breaks substitutability (a caller holding an A that invokes `foo` fails when
# handed a B), so the rule fires on B#foo, anchored on its name token.
class A
  def foo; end
end

class B < A
  private

  def foo; end
end
