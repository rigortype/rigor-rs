# Singleton-side mixins and unknown-DSL macros (upstream #2269 / #2094, v0.4.6).
#
# Once RBS says a class method is absent, `call.undefined-method` declines when
# the receiver class, or an RBS superclass of it, has a singleton-side mixin
# that may answer: an unrecordable rewrite of its singleton class
# (`IO.singleton_class.send(:define_method, …)` taints IO, so File too), a
# non-constant `extend`, or an extended module that defines the name,
# macro-names it (`some_macro :named`), or extends an unknown DSL. The
# instance side gains the same macro / unknown-DSL arms: String includes a
# module extending an unknown DSL, so every `"s".name` declines.
#
# Must-still-fire controls: a direct `singleton_class.define_method(:x)` does
# not taint (`Hash.zzz_h`), an untouched class (`Integer.zzz_i`), a name no
# extended module answers (`Rational.zzz_r`, `Symbol.zzz_s`, `[1].zzz_arr`),
# and an Object-level extend read only for the names it defines (`Float.zzz_f`).

IO.singleton_class.send(:define_method, :select) { |*params| params }
Hash.singleton_class.define_method(:hsel) { 1 }

module Plain
  def pl = 1
end
Rational.extend(Plain)

module Named
  some_macro :mac_named
end
Symbol.extend(Named)

module OpenDsl
  extend UnknownGem::Dsl
end
Range.extend(OpenDsl)

module ObjNamed
  some_macro :obj_named
end
Object.extend(ObjNamed)

module StrMac
  extend UnknownGem::Dsl
  other_macro :str_named
end
String.include(StrMac)

m = Plain
Complex.extend(m)

File.stubs(:file?)
File.zzz_file
IO.zzz_io
Hash.zzz_h
Integer.zzz_i
Rational.pl
Rational.zzz_r
Symbol.mac_named
Symbol.zzz_s
Range.anything_goes
Float.obj_named
Float.zzz_f
"s".str_named
"s".zzz_str
Complex.pl
Array.zzz_a
[1].zzz_arr
