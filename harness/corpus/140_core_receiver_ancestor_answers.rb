# A core receiver the program may answer through an RBS ancestor or a
# project subclass (upstream #2086 / issue #1722, v0.4.5).
#
# Once RBS says the method is absent, `call.undefined-method` declines when a
# class on the receiver's RBS ancestor chain has a project `method_missing`,
# or a project module that defines the name is mixed into that chain — by a
# receiver-form `X.include(M)` / `X.prepend(M)` / `X.send(:include, M)`
# wherever it is written. A receiver typed by its class's name (not a literal,
# a shape, or a receiver written as a literal) also declines when a project
# subclass defines the name.
#
# Must-still-fire controls: a receiver whose chain holds no such module
# (`{ a: 1 }.zzz_arr`, `[1].zzz` — Array is not Comparable), a name no mixin
# defines (`[1].nope`, `{ a: 1 }.nope`), and a shape receiver beside a
# project subclass that defines the name (`{ a: 1 }.hsub`).

module Comparable
  def method_missing(*) = 1
end

module ArrExt
  def zzz_arr = 1
end
Array.include(ArrExt)

module SymExt
  def zzz_sym = 1
end
Symbol.send(:include, SymExt)

module HashExt
  def zzz_hash = 1
end
Hash.prepend(HashExt)

class HSub < Hash
  def hsub = 1
end

1.zzz
"a".zzz
[1].zzz_arr
:a.zzz_sym
{ a: 1 }.zzz_hash

[1].zzz
[1].nope
{ a: 1 }.zzz_arr
{ a: 1 }.hsub
{ a: 1 }.nope
